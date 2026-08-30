//! How MCP server activity reaches an ACP client.
//!
//! Progress, elicitation, catalog changes and auth prompts all cross the
//! adapter as `ext` session updates, so they are asserted together: the shape
//! one of them projects is the shape the others have to match.

use super::*;

#[tokio::test(flavor = "current_thread")]
async fn mcp_progress_notification_reaches_acp_as_ext_event() {
    let actual = collect_notifications(vec![AgentEvent::McpNotification {
        session_id: "session-1".to_string(),
        server: "filesystem".to_string(),
        method: "notifications/progress".to_string(),
        direction: "notification".to_string(),
        params: serde_json::json!({
            "progressToken": "tok-1",
            "progress": 42.0,
            "total": 100.0,
            "server": "filesystem",
            "tool": "search_files"
        }),
    }])
    .await;

    let notification = &actual[0];
    assert_eq!(notification["method"], HARN_AGENT_EVENT_METHOD);
    let params = &notification["params"];
    assert_eq!(params["kind"], "mcp_notification");
    assert_eq!(params["sessionId"], "session-1");
    assert_eq!(params["server"], "filesystem");
    assert_eq!(params["method"], "notifications/progress");
    assert_eq!(params["direction"], "notification");
    assert_eq!(params["params"]["progress"], 42.0);
    assert_eq!(params["params"]["progressToken"], "tok-1");
    assert!(
        HARN_AGENT_EVENT_KINDS.contains(&"mcp_notification"),
        "mcp_notification must be advertised so clients can subscribe"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn mcp_elicitation_request_reaches_acp_as_ext_event() {
    let actual = collect_notifications(vec![AgentEvent::McpNotification {
        session_id: "session-1".to_string(),
        server: "deploy-bot".to_string(),
        method: "elicitation/create".to_string(),
        direction: "request".to_string(),
        params: serde_json::json!({
            "message": "Confirm production deploy?",
            "requestedSchema": {"type": "object"}
        }),
    }])
    .await;

    let notification = &actual[0];
    assert_eq!(notification["method"], HARN_AGENT_EVENT_METHOD);
    let params = &notification["params"];
    assert_eq!(params["kind"], "mcp_notification");
    assert_eq!(params["direction"], "request");
    assert_eq!(params["method"], "elicitation/create");
    assert_eq!(params["params"]["message"], "Confirm production deploy?");
}

#[tokio::test(flavor = "current_thread")]
async fn mcp_catalog_changed_reaches_acp_as_ext_event() {
    let actual = collect_notifications(vec![AgentEvent::McpCatalogChanged {
        session_id: "session-1".to_string(),
        server: Some("github".to_string()),
        reason: "list_changed".to_string(),
    }])
    .await;

    let notification = &actual[0];
    assert_eq!(notification["method"], HARN_AGENT_EVENT_METHOD);
    let params = &notification["params"];
    assert_eq!(params["kind"], "mcp_catalog_changed");
    assert_eq!(params["sessionId"], "session-1");
    assert_eq!(params["server"], "github");
    assert_eq!(params["reason"], "list_changed");
    assert!(
        HARN_AGENT_EVENT_KINDS.contains(&"mcp_catalog_changed"),
        "mcp_catalog_changed must be advertised so clients can subscribe"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn mcp_catalog_changed_allows_serverless_allowlist_update() {
    let actual = collect_notifications(vec![AgentEvent::McpCatalogChanged {
        session_id: "session-1".to_string(),
        server: None,
        reason: "allowlist_updated".to_string(),
    }])
    .await;

    let params = &actual[0]["params"];
    assert_eq!(params["kind"], "mcp_catalog_changed");
    assert!(params["server"].is_null());
    assert_eq!(params["reason"], "allowlist_updated");
}

#[tokio::test(flavor = "current_thread")]
async fn mcp_auth_required_reaches_acp_as_ext_event() {
    let actual = collect_notifications(vec![AgentEvent::McpAuthRequired {
        session_id: "session-1".to_string(),
        server: "notion".to_string(),
        resource: "https://mcp.notion.com".to_string(),
        scope: Some("read write".to_string()),
    }])
    .await;

    let notification = &actual[0];
    assert_eq!(notification["method"], HARN_AGENT_EVENT_METHOD);
    let params = &notification["params"];
    assert_eq!(params["kind"], "mcp_auth_required");
    assert_eq!(params["sessionId"], "session-1");
    assert_eq!(params["server"], "notion");
    assert_eq!(params["resource"], "https://mcp.notion.com");
    assert_eq!(params["scope"], "read write");
    assert!(
        HARN_AGENT_EVENT_KINDS.contains(&"mcp_auth_required"),
        "mcp_auth_required must be advertised so clients can subscribe"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn mcp_auth_required_omits_absent_scope() {
    let actual = collect_notifications(vec![AgentEvent::McpAuthRequired {
        session_id: "session-1".to_string(),
        server: "notion".to_string(),
        resource: "https://mcp.notion.com".to_string(),
        scope: None,
    }])
    .await;

    let params = &actual[0]["params"];
    assert_eq!(params["kind"], "mcp_auth_required");
    assert!(params["scope"].is_null());
}
