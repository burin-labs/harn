use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use super::card::{agent_card, pipeline_name_from_path};
use super::http::{check_version_header, parse_http_request};
use super::rpc::{
    error_response, extract_message_params, handle_jsonrpc, A2A_TASK_NOT_FOUND,
    A2A_UNSUPPORTED_OPERATION,
};
use super::task::{
    cancel_task, complete_task, create_task, list_tasks, mark_task_working, Artifact, TaskMessage,
    TaskState, TaskStatus, TaskStore,
};

#[test]
fn test_agent_card_v1_fields() {
    let card = agent_card("test-pipeline", 8080);
    assert_eq!(card["id"], "test-pipeline");
    assert_eq!(card["name"], "test-pipeline");
    assert!(card["provider"]["organization"].is_string());
    assert!(card["provider"]["url"].is_string());
    assert!(card["interfaces"].is_array());
    assert_eq!(card["interfaces"][0]["protocol"], "jsonrpc");
    assert!(card["securitySchemes"].is_array());
    assert_eq!(card["securitySchemes"].as_array().unwrap().len(), 0);
    assert_eq!(card["capabilities"]["streaming"], true);
    assert_eq!(card["capabilities"]["pushNotifications"], false);
    assert_eq!(card["capabilities"]["extendedAgentCard"], false);
    // v0.3 fields should not be present
    assert!(card.get("defaultInputModes").is_none());
    assert!(card.get("defaultOutputModes").is_none());
}

#[test]
fn test_agent_card_url() {
    let card = agent_card("my-agent", 3000);
    assert_eq!(card["url"], "http://localhost:3000");
}

#[test]
fn test_task_status_str() {
    assert_eq!(TaskStatus::Submitted.as_str(), "submitted");
    assert_eq!(TaskStatus::Working.as_str(), "working");
    assert_eq!(TaskStatus::Completed.as_str(), "completed");
    assert_eq!(TaskStatus::Failed.as_str(), "failed");
    assert_eq!(TaskStatus::Cancelled.as_str(), "cancelled");
    assert_eq!(TaskStatus::Rejected.as_str(), "rejected");
    assert_eq!(TaskStatus::InputRequired.as_str(), "input-required");
    assert_eq!(TaskStatus::AuthRequired.as_str(), "auth-required");
}

#[test]
fn test_task_status_terminal() {
    assert!(!TaskStatus::Submitted.is_terminal());
    assert!(!TaskStatus::Working.is_terminal());
    assert!(TaskStatus::Completed.is_terminal());
    assert!(TaskStatus::Failed.is_terminal());
    assert!(TaskStatus::Cancelled.is_terminal());
    assert!(TaskStatus::Rejected.is_terminal());
    assert!(!TaskStatus::InputRequired.is_terminal());
}

#[test]
fn test_create_task_generates_uuid() {
    let store: TaskStore = Arc::new(Mutex::new(HashMap::new()));
    let id = create_task(&store, "hello", None);
    // UUID v7 format: 8-4-4-4-12 hex chars
    assert_eq!(id.len(), 36);
    assert!(id.contains('-'));
    // Verify it's in the store
    let map = store.lock().unwrap();
    let task = map.get(&id).unwrap();
    assert_eq!(task.status, TaskStatus::Submitted);
    assert_eq!(task.history.len(), 1);
    assert_eq!(task.history[0].role, "user");
    // Message should have an id too
    assert_eq!(task.history[0].id.len(), 36);
    assert!(task.context_id.is_none());
}

#[test]
fn test_create_task_with_context_id() {
    let store: TaskStore = Arc::new(Mutex::new(HashMap::new()));
    let id = create_task(&store, "hello", Some("ctx-123".to_string()));
    let map = store.lock().unwrap();
    let task = map.get(&id).unwrap();
    assert_eq!(task.context_id, Some("ctx-123".to_string()));
}

#[test]
fn test_task_to_json_includes_context_id() {
    let task = TaskState {
        id: "task-1".to_string(),
        context_id: Some("ctx-abc".to_string()),
        status: TaskStatus::Submitted,
        history: vec![],
        artifacts: vec![],
    };
    let json = task.to_json();
    assert_eq!(json["contextId"], "ctx-abc");
}

#[test]
fn test_task_to_json_without_context_id() {
    let task = TaskState {
        id: "task-1".to_string(),
        context_id: None,
        status: TaskStatus::Submitted,
        history: vec![],
        artifacts: vec![],
    };
    let json = task.to_json();
    assert!(json.get("contextId").is_none());
}

#[test]
fn test_task_message_includes_id() {
    let task = TaskState {
        id: "task-1".to_string(),
        context_id: None,
        status: TaskStatus::Completed,
        history: vec![TaskMessage {
            id: "msg-abc".to_string(),
            role: "user".to_string(),
            parts: vec![serde_json::json!({"type": "text", "text": "hi"})],
        }],
        artifacts: vec![],
    };
    let json = task.to_json();
    assert_eq!(json["history"][0]["id"], "msg-abc");
    assert_eq!(json["history"][0]["role"], "user");
}

#[test]
fn test_artifact_to_json() {
    let artifact = Artifact {
        id: "art-1".to_string(),
        title: Some("Output".to_string()),
        description: Some("Pipeline output".to_string()),
        mime_type: Some("text/plain".to_string()),
        parts: vec![serde_json::json!({"type": "text", "text": "hello"})],
    };
    let json = artifact.to_json();
    assert_eq!(json["id"], "art-1");
    assert_eq!(json["title"], "Output");
    assert_eq!(json["description"], "Pipeline output");
    assert_eq!(json["mimeType"], "text/plain");
    assert_eq!(json["parts"][0]["type"], "text");
}

#[test]
fn test_artifact_to_json_minimal() {
    let artifact = Artifact {
        id: "art-2".to_string(),
        title: None,
        description: None,
        mime_type: None,
        parts: vec![],
    };
    let json = artifact.to_json();
    assert_eq!(json["id"], "art-2");
    assert!(json.get("title").is_none());
    assert!(json.get("description").is_none());
    assert!(json.get("mimeType").is_none());
}

#[test]
fn test_task_to_json_includes_artifacts() {
    let task = TaskState {
        id: "task-1".to_string(),
        context_id: None,
        status: TaskStatus::Completed,
        history: vec![],
        artifacts: vec![Artifact {
            id: "art-1".to_string(),
            title: Some("Result".to_string()),
            description: None,
            mime_type: None,
            parts: vec![serde_json::json!({"type": "text", "text": "output"})],
        }],
    };
    let json = task.to_json();
    assert_eq!(json["artifacts"][0]["id"], "art-1");
    assert_eq!(json["artifacts"][0]["title"], "Result");
}

#[test]
fn test_cancel_task_success() {
    let store: TaskStore = Arc::new(Mutex::new(HashMap::new()));
    let id = create_task(&store, "hello", None);
    mark_task_working(&store, &id);
    let result = cancel_task(&store, &id);
    assert!(result.is_ok());
    let json = result.unwrap();
    assert_eq!(json["status"]["state"], "cancelled");
}

#[test]
fn test_cancel_task_terminal_fails() {
    let store: TaskStore = Arc::new(Mutex::new(HashMap::new()));
    let id = create_task(&store, "hello", None);
    complete_task(&store, &id, "done");
    let result = cancel_task(&store, &id);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("TaskNotCancelableError"));
}

#[test]
fn test_cancel_task_not_found() {
    let store: TaskStore = Arc::new(Mutex::new(HashMap::new()));
    let result = cancel_task(&store, "nonexistent");
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("TaskNotFoundError"));
}

#[test]
fn test_list_tasks_empty() {
    let store: TaskStore = Arc::new(Mutex::new(HashMap::new()));
    let result = list_tasks(&store, None, None);
    assert_eq!(result["tasks"].as_array().unwrap().len(), 0);
    assert!(result.get("nextCursor").is_none());
}

#[test]
fn test_list_tasks_returns_summaries() {
    let store: TaskStore = Arc::new(Mutex::new(HashMap::new()));
    create_task(&store, "task1", Some("ctx-1".to_string()));
    create_task(&store, "task2", None);
    let result = list_tasks(&store, None, None);
    let tasks = result["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 2);
    // Summaries should have id and status but not full history
    for t in tasks {
        assert!(t.get("id").is_some());
        assert!(t.get("status").is_some());
        assert!(t.get("history").is_none());
    }
}

#[test]
fn test_list_tasks_pagination() {
    let store: TaskStore = Arc::new(Mutex::new(HashMap::new()));
    let mut ids = Vec::new();
    for i in 0..5 {
        ids.push(create_task(&store, &format!("task{i}"), None));
    }
    // Get first 2
    let result = list_tasks(&store, None, Some(2));
    let tasks = result["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 2);
    // Should have a nextCursor
    assert!(result.get("nextCursor").is_some());
}

#[test]
fn test_task_summary_json() {
    let task = TaskState {
        id: "task-1".to_string(),
        context_id: Some("ctx-abc".to_string()),
        status: TaskStatus::Working,
        history: vec![TaskMessage {
            id: "msg-1".to_string(),
            role: "user".to_string(),
            parts: vec![serde_json::json!({"type": "text", "text": "hello"})],
        }],
        artifacts: vec![],
    };
    let summary = task.to_summary_json();
    assert_eq!(summary["id"], "task-1");
    assert_eq!(summary["status"]["state"], "working");
    assert_eq!(summary["contextId"], "ctx-abc");
    // Summary should not include history
    assert!(summary.get("history").is_none());
}

#[test]
fn test_check_version_header_ok_no_header() {
    let headers = HashMap::new();
    let rpc_id = serde_json::Value::Number(1.into());
    assert!(check_version_header(&headers, &rpc_id).is_none());
}

#[test]
fn test_check_version_header_ok_matching() {
    let mut headers = HashMap::new();
    headers.insert("a2a-version".to_string(), "1.0.0".to_string());
    let rpc_id = serde_json::Value::Number(1.into());
    assert!(check_version_header(&headers, &rpc_id).is_none());
}

#[test]
fn test_check_version_header_unsupported() {
    let mut headers = HashMap::new();
    headers.insert("a2a-version".to_string(), "0.3".to_string());
    let rpc_id = serde_json::Value::Number(1.into());
    let err = check_version_header(&headers, &rpc_id);
    assert!(err.is_some());
    let err = err.unwrap();
    assert_eq!(err["error"]["code"], -32009);
    assert!(err["error"]["message"]
        .as_str()
        .unwrap()
        .contains("VersionNotSupportedError"));
}

#[test]
fn test_parse_http_request_with_headers() {
    let raw = b"POST / HTTP/1.1\r\nContent-Type: application/json\r\nA2A-Version: 1.0.0\r\n\r\n{\"test\":true}";
    let req = parse_http_request(raw).unwrap();
    assert_eq!(req.method, "POST");
    assert_eq!(req.path, "/");
    assert_eq!(req.headers.get("a2a-version").unwrap(), "1.0.0");
    assert_eq!(req.headers.get("content-type").unwrap(), "application/json");
    assert_eq!(req.body, "{\"test\":true}");
}

#[test]
fn test_error_response_format() {
    let resp = error_response(&serde_json::Value::Number(42.into()), -32009, "test error");
    assert_eq!(resp["jsonrpc"], "2.0");
    assert_eq!(resp["id"], 42);
    assert_eq!(resp["error"]["code"], -32009);
    assert_eq!(resp["error"]["message"], "test error");
}

#[test]
fn test_pipeline_name_from_path() {
    assert_eq!(pipeline_name_from_path("examples/hello.harn"), "hello");
    assert_eq!(pipeline_name_from_path("agent.harn"), "agent");
    assert_eq!(
        pipeline_name_from_path("/path/to/my-pipeline.harn"),
        "my-pipeline"
    );
}

#[test]
fn test_extract_message_params() {
    let parsed = serde_json::json!({
        "params": {
            "message": {
                "parts": [{"type": "text", "text": "hello world"}]
            },
            "contextId": "ctx-123"
        }
    });
    let (text, ctx) = extract_message_params(&parsed);
    assert_eq!(text, "hello world");
    assert_eq!(ctx, Some("ctx-123".to_string()));
}

#[test]
fn test_extract_message_params_no_context() {
    let parsed = serde_json::json!({
        "params": {
            "message": {
                "parts": [{"type": "text", "text": "hello"}]
            }
        }
    });
    let (text, ctx) = extract_message_params(&parsed);
    assert_eq!(text, "hello");
    assert!(ctx.is_none());
}

#[tokio::test]
async fn test_handle_jsonrpc_unsupported_method() {
    let store: TaskStore = Arc::new(Mutex::new(HashMap::new()));
    let body = r#"{"jsonrpc":"2.0","id":1,"method":"old/method","params":{}}"#;
    let resp = handle_jsonrpc("/nonexistent.harn", body, &store).await;
    let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
    assert_eq!(parsed["error"]["code"], A2A_UNSUPPORTED_OPERATION);
    assert!(parsed["error"]["message"]
        .as_str()
        .unwrap()
        .contains("old/method"));
}

#[tokio::test]
async fn test_handle_jsonrpc_old_method_names_rejected() {
    let store: TaskStore = Arc::new(Mutex::new(HashMap::new()));

    // Old v0.3 method names should be rejected
    for method in &["message/send", "task/get", "task/cancel"] {
        let body = format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"{}","params":{{}}}}"#,
            method
        );
        let resp = handle_jsonrpc("/nonexistent.harn", &body, &store).await;
        let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(
            parsed["error"]["code"], A2A_UNSUPPORTED_OPERATION,
            "Old method {method} should be rejected"
        );
    }
}

#[tokio::test]
async fn test_handle_jsonrpc_parse_error() {
    let store: TaskStore = Arc::new(Mutex::new(HashMap::new()));
    let resp = handle_jsonrpc("/nonexistent.harn", "not json", &store).await;
    let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
    assert_eq!(parsed["error"]["code"], -32700);
}

#[tokio::test]
async fn test_handle_jsonrpc_get_task_not_found() {
    let store: TaskStore = Arc::new(Mutex::new(HashMap::new()));
    let body = r#"{"jsonrpc":"2.0","id":1,"method":"a2a.GetTask","params":{"id":"nonexistent"}}"#;
    let resp = handle_jsonrpc("/nonexistent.harn", body, &store).await;
    let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
    assert_eq!(parsed["error"]["code"], A2A_TASK_NOT_FOUND);
}

#[tokio::test]
async fn test_handle_jsonrpc_list_tasks() {
    let store: TaskStore = Arc::new(Mutex::new(HashMap::new()));
    create_task(&store, "test1", None);
    create_task(&store, "test2", Some("ctx".to_string()));
    let body = r#"{"jsonrpc":"2.0","id":1,"method":"a2a.ListTasks","params":{}}"#;
    let resp = handle_jsonrpc("/nonexistent.harn", body, &store).await;
    let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
    assert!(parsed.get("error").is_none());
    let tasks = parsed["result"]["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 2);
}

#[tokio::test]
async fn test_handle_jsonrpc_send_message_empty() {
    let store: TaskStore = Arc::new(Mutex::new(HashMap::new()));
    let body =
        r#"{"jsonrpc":"2.0","id":1,"method":"a2a.SendMessage","params":{"message":{"parts":[]}}}"#;
    let resp = handle_jsonrpc("/nonexistent.harn", body, &store).await;
    let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
    assert_eq!(parsed["error"]["code"], -32602);
}
