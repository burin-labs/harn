//! Shared JSON-RPC 2.0 message construction helpers.

/// Build a JSON-RPC 2.0 request object.
#[inline]
pub fn request(
    id: impl Into<serde_json::Value>,
    method: &str,
    params: serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id.into(),
        "method": method,
        "params": params,
    })
}

/// Build a JSON-RPC 2.0 notification (no id).
#[inline]
pub fn notification(method: &str, params: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params,
    })
}

/// Build a JSON-RPC 2.0 success response.
#[inline]
pub fn response(id: impl Into<serde_json::Value>, result: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id.into(),
        "result": result,
    })
}

/// Build a JSON-RPC 2.0 error response.
#[inline]
pub fn error_response(
    id: impl Into<serde_json::Value>,
    code: i64,
    message: &str,
) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id.into(),
        "error": { "code": code, "message": message },
    })
}

/// Build a JSON-RPC 2.0 error response with additional data.
#[inline]
pub fn error_response_with_data(
    id: impl Into<serde_json::Value>,
    code: i64,
    message: &str,
    data: serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id.into(),
        "error": { "code": code, "message": message, "data": data },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_has_all_fields() {
        let req = request(1, "foo/bar", serde_json::json!({"key": "val"}));
        assert_eq!(req["jsonrpc"], "2.0");
        assert_eq!(req["id"], 1);
        assert_eq!(req["method"], "foo/bar");
        assert_eq!(req["params"]["key"], "val");
    }

    #[test]
    fn notification_has_no_id() {
        let notif = notification("update", serde_json::json!({}));
        assert_eq!(notif["jsonrpc"], "2.0");
        assert_eq!(notif["method"], "update");
        assert!(notif.get("id").is_none());
    }

    #[test]
    fn response_wraps_result() {
        let resp = response(42, serde_json::json!({"data": true}));
        assert_eq!(resp["id"], 42);
        assert_eq!(resp["result"]["data"], true);
    }

    #[test]
    fn error_response_wraps_error() {
        let resp = error_response(1, -32600, "Invalid Request");
        assert_eq!(resp["error"]["code"], -32600);
        assert_eq!(resp["error"]["message"], "Invalid Request");
    }

    #[test]
    fn error_response_with_data_includes_data() {
        let resp =
            error_response_with_data(1, -32602, "Bad params", serde_json::json!({"field": "x"}));
        assert_eq!(resp["error"]["data"]["field"], "x");
    }

    #[test]
    fn request_accepts_string_id() {
        let req = request("abc-123", "test", serde_json::json!(null));
        assert_eq!(req["id"], "abc-123");
    }
}
