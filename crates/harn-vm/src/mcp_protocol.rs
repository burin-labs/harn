//! Shared MCP protocol-version and feature-gap helpers.

use serde_json::{json, Value as JsonValue};

pub const PROTOCOL_VERSION: &str = "2025-11-25";
pub const METHOD_TASKS_GET: &str = "tasks/get";
pub const METHOD_TASKS_RESULT: &str = "tasks/result";
pub const METHOD_TASKS_LIST: &str = "tasks/list";
pub const METHOD_TASKS_CANCEL: &str = "tasks/cancel";
pub const METHOD_TASK_STATUS_NOTIFICATION: &str = "notifications/tasks/status";
pub const METHOD_ROOTS_LIST: &str = "roots/list";
pub const METHOD_ROOTS_LIST_CHANGED_NOTIFICATION: &str = "notifications/roots/list_changed";
pub const RELATED_TASK_META_KEY: &str = "io.modelcontextprotocol/related-task";
pub const DEFAULT_TASK_POLL_INTERVAL_MS: u64 = 250;
pub const DEFAULT_MCP_LIST_PAGE_SIZE: usize = 100;
pub const MCP_LIST_PAGE_SIZE_ENV: &str = "HARN_MCP_LIST_PAGE_SIZE";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpListPage {
    pub start: usize,
    pub end: usize,
    pub next_cursor: Option<String>,
}

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
    // `sampling/createMessage` (client) is supported — handled in
    // `mcp::handle_inbound_client_request` via
    // `mcp_sampling::dispatch_inbound_sampling`, which routes the
    // request through the host bridge's `("mcp", "sample")` operation
    // and on to Harn's `llm_call`. Intentionally omitted from this
    // gap list.
    //
    // `elicitation/create` is supported on both roles — handled in
    // `mcp::stdio_call`/`mcp::http_call` (client) and via `mcp_elicit(...)`
    // (server). It is intentionally omitted from this gap list.
    //
    // `roots/list` is supported when Harn acts as an MCP client and answers
    // inbound server-to-client root discovery requests. It is intentionally
    // omitted from this gap list.
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpTaskStatus {
    Working,
    InputRequired,
    Completed,
    Failed,
    Cancelled,
}

impl McpTaskStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::InputRequired => "input_required",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpToolTaskSupport {
    Required,
    Optional,
    Forbidden,
}

impl McpToolTaskSupport {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Required => "required",
            Self::Optional => "optional",
            Self::Forbidden => "forbidden",
        }
    }
}

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
    task_augmentation_error_response(
        id,
        method,
        -32602,
        "MCP task-augmented execution is not supported",
        "Harn MCP tools execute inline and do not advertise taskSupport.",
    )
}

pub fn task_augmentation_error_response(
    id: impl Into<JsonValue>,
    method: &str,
    code: i64,
    message: &str,
    reason: &str,
) -> JsonValue {
    crate::jsonrpc::error_response_with_data(
        id,
        code,
        message,
        json!({
            "type": "mcp.unsupportedFeature",
            "protocolVersion": PROTOCOL_VERSION,
            "method": method,
            "feature": "tasks",
            "status": "unsupported",
            "reason": reason,
        }),
    )
}

pub fn requests_task_augmentation(params: &JsonValue) -> bool {
    params.get("task").is_some()
}

pub fn tasks_capability() -> JsonValue {
    json!({
        "list": {},
        "cancel": {},
        "requests": {
            "tools": {
                "call": {}
            }
        }
    })
}

pub fn tool_execution(task_support: McpToolTaskSupport) -> JsonValue {
    json!({
        "taskSupport": task_support.as_str(),
    })
}

pub fn related_task_meta(task_id: &str) -> JsonValue {
    json!({
        RELATED_TASK_META_KEY: {
            "taskId": task_id,
        }
    })
}

pub fn mcp_list_page_size() -> usize {
    mcp_list_page_size_from_env(std::env::var(MCP_LIST_PAGE_SIZE_ENV).ok().as_deref())
}

fn mcp_list_page_size_from_env(raw: Option<&str>) -> usize {
    raw.and_then(|value| value.parse::<usize>().ok())
        .filter(|size| *size > 0)
        .unwrap_or(DEFAULT_MCP_LIST_PAGE_SIZE)
}

pub fn encode_mcp_list_cursor(offset: usize) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(offset.to_string().as_bytes())
}

pub fn mcp_list_page(
    params: &JsonValue,
    total_len: usize,
    method: &str,
) -> Result<McpListPage, String> {
    let offset = parse_mcp_list_cursor(params, method)?;
    let page_size = mcp_list_page_size();
    let start = offset.min(total_len);
    let end = start.saturating_add(page_size).min(total_len);
    let next_cursor = (end < total_len).then(|| encode_mcp_list_cursor(end));
    Ok(McpListPage {
        start,
        end,
        next_cursor,
    })
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

fn parse_mcp_list_cursor(params: &JsonValue, method: &str) -> Result<usize, String> {
    let Some(cursor) = params.get("cursor") else {
        return Ok(0);
    };
    let Some(cursor) = cursor.as_str() else {
        return Err(format!("invalid {method} cursor"));
    };
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(cursor)
        .map_err(|_| format!("invalid {method} cursor"))?;
    let decoded = String::from_utf8(bytes).map_err(|_| format!("invalid {method} cursor"))?;
    decoded
        .parse::<usize>()
        .map_err(|_| format!("invalid {method} cursor"))
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
        ] {
            let response = unsupported_latest_spec_method_response(json!(1), method)
                .expect("expected explicit unsupported method");
            assert_eq!(response["error"]["code"], json!(-32601));
            assert_eq!(response["error"]["data"]["method"], json!(method));
            assert_eq!(response["error"]["data"]["status"], json!("unsupported"));
        }
    }

    #[test]
    fn elicitation_create_is_no_longer_in_the_unsupported_gap_list() {
        // Removed from `UNSUPPORTED_LATEST_SPEC_METHODS` once we
        // implemented bidirectional elicitation (issue #875). Lookup
        // therefore returns `None` and callers route the method
        // through the elicitation bus (server) or the host bridge
        // (client) instead of the auto-rejection path.
        assert!(unsupported_latest_spec_method("elicitation/create").is_none());
    }

    #[test]
    fn roots_list_is_no_longer_in_the_unsupported_gap_list() {
        assert!(unsupported_latest_spec_method(METHOD_ROOTS_LIST).is_none());
    }

    #[test]
    fn sampling_create_message_is_no_longer_in_the_unsupported_gap_list() {
        // Removed from `UNSUPPORTED_LATEST_SPEC_METHODS` once we
        // implemented inbound server-to-client sampling (issue #874).
        // Lookup therefore returns `None` and callers route the method
        // through `mcp_sampling::dispatch_inbound_sampling` on the
        // client side instead of the auto-rejection path.
        assert!(unsupported_latest_spec_method("sampling/createMessage").is_none());
    }

    #[test]
    fn task_augmentation_error_is_json_rpc_shaped() {
        let response = unsupported_task_augmentation_response(json!("call-1"), "tools/call");
        assert_eq!(response["jsonrpc"], json!("2.0"));
        assert_eq!(response["id"], json!("call-1"));
        assert_eq!(response["error"]["code"], json!(-32602));
        assert_eq!(response["error"]["data"]["feature"], json!("tasks"));
    }

    #[test]
    fn task_protocol_shapes_match_latest_spec_names() {
        assert_eq!(McpTaskStatus::Working.as_str(), "working");
        assert_eq!(McpTaskStatus::InputRequired.as_str(), "input_required");
        assert!(McpTaskStatus::Completed.is_terminal());
        assert_eq!(tasks_capability()["requests"]["tools"]["call"], json!({}));
        assert_eq!(
            tool_execution(McpToolTaskSupport::Optional)["taskSupport"],
            json!("optional")
        );
        assert_eq!(
            related_task_meta("task-1")[RELATED_TASK_META_KEY]["taskId"],
            json!("task-1")
        );
    }

    #[test]
    fn mcp_list_page_uses_default_size_and_next_cursor() {
        let page = mcp_list_page(&json!({}), 105, "tools/list").unwrap();
        assert_eq!(page.start, 0);
        assert_eq!(page.end, DEFAULT_MCP_LIST_PAGE_SIZE);
        assert_eq!(
            page.next_cursor,
            Some(encode_mcp_list_cursor(DEFAULT_MCP_LIST_PAGE_SIZE))
        );

        let next = mcp_list_page(
            &json!({"cursor": page.next_cursor.unwrap()}),
            105,
            "tools/list",
        )
        .unwrap();
        assert_eq!(next.start, DEFAULT_MCP_LIST_PAGE_SIZE);
        assert_eq!(next.end, 105);
        assert_eq!(next.next_cursor, None);
    }

    #[test]
    fn mcp_list_page_size_parses_positive_env_override() {
        assert_eq!(mcp_list_page_size_from_env(Some("2")), 2);
        assert_eq!(
            mcp_list_page_size_from_env(Some("0")),
            DEFAULT_MCP_LIST_PAGE_SIZE
        );
        assert_eq!(
            mcp_list_page_size_from_env(Some("nope")),
            DEFAULT_MCP_LIST_PAGE_SIZE
        );
        assert_eq!(
            mcp_list_page_size_from_env(None),
            DEFAULT_MCP_LIST_PAGE_SIZE
        );
    }

    #[test]
    fn mcp_list_page_rejects_malformed_cursor() {
        let err = mcp_list_page(&json!({"cursor": "not-base64"}), 5, "resources/list")
            .expect_err("malformed cursor should fail");
        assert_eq!(err, "invalid resources/list cursor");
    }
}
