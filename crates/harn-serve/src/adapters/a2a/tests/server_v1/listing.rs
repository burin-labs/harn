use super::*;

#[tokio::test]
async fn list_filters_pages_and_projects_real_completed_tasks() {
    let (_dir, server) = echo_server();
    let router = http_router(server.clone(), LOCAL_URL);
    let mut ids = Vec::new();
    for context in ["shared", "other", "shared"] {
        let mut params = send_params();
        params["message"]["contextId"] = json!(context);
        let (_, sent) = rpc(router.clone(), "1.0", "SendMessage", params).await;
        let task = &sent["result"]["task"];
        assert_eq!(task["status"]["state"], "TASK_STATE_COMPLETED", "{sent}");
        assert_eq!(task["history"][1]["parts"][0]["text"], "hello");
        assert!(!task["artifacts"].as_array().unwrap().is_empty());
        ids.push(task["id"].as_str().unwrap().to_owned());
    }
    let options = json!({"contextId":"shared", "status":"TASK_STATE_COMPLETED", "pageSize":1,
        "historyLength":0, "statusTimestampAfter":"1970-01-01T00:00:00Z"});
    let (_, first) = rpc(router.clone(), "1.0", "ListTasks", options.clone()).await;
    let first = &first["result"];
    assert_upstream("ListTasksResponse", first);
    assert_eq!(first["totalSize"], 2);
    assert_eq!(first["pageSize"], 1);
    assert_eq!(first["tasks"].as_array().unwrap().len(), 1);
    let item = &first["tasks"][0];
    assert_eq!(item["id"], ids[2]);
    assert_eq!(item["history"], json!([]));
    assert!(item.get("artifacts").is_none());
    assert!(item["status"]["timestamp"].is_string());
    let token = first["nextPageToken"].as_str().unwrap();
    assert!(
        !token.is_empty(),
        "first page must expose the actual continuation"
    );
    let mut next = options.clone();
    next["pageToken"] = json!(token);
    let (_, second) = rpc(router.clone(), "1.0", "ListTasks", next).await;
    let second = &second["result"];
    assert_upstream("ListTasksResponse", second);
    assert_eq!(second["tasks"][0]["id"], ids[0]);
    assert_eq!(second["nextPageToken"], "");
    let (_, all) = rpc(
        router.clone(),
        "1.0",
        "ListTasks",
        json!({"includeArtifacts":true,"historyLength":1}),
    )
    .await;
    assert_eq!(all["result"]["totalSize"], 3);
    assert_eq!(all["result"]["pageSize"], 50);
    for task in all["result"]["tasks"].as_array().unwrap() {
        assert_eq!(task["history"].as_array().unwrap().len(), 1);
        assert!(!task["artifacts"].as_array().unwrap().is_empty());
    }
    for filter in [
        json!({"contextId":"missing"}),
        json!({"status":"TASK_STATE_FAILED"}),
        json!({"statusTimestampAfter":"9999-01-01T00:00:00Z"}),
    ] {
        let (_, result) = rpc(router.clone(), "1.0", "ListTasks", filter).await;
        assert_eq!(result["result"]["tasks"], json!([]), "{result}");
        assert_eq!(result["result"]["totalSize"], 0);
        assert_eq!(result["result"]["nextPageToken"], "");
    }
    for invalid in [
        json!({"pageSize":0}),
        json!({"pageSize":101}),
        json!({"historyLength":-1}),
        json!({"status":"completed"}),
        json!({"includeArtifacts":"true"}),
        json!({"statusTimestampAfter":"yesterday"}),
        json!({"pageToken":"broken"}),
        json!({"pageToken":token,"contextId":"other"}),
    ] {
        let (_, refused) = rpc(router.clone(), "1.0", "ListTasks", invalid).await;
        assert_eq!(refused["error"]["code"], -32602, "{refused}");
    }
    assert_eq!(
        server.tasks.lock().unwrap().len(),
        3,
        "listing must allocate no tasks"
    );
}

#[tokio::test]
async fn authenticated_tasks_are_scoped_by_trusted_principal() {
    let (_dir, server) = test_server_with(ECHO_SCRIPT, |config| {
        config.auth_policy = AuthPolicy {
            methods: vec![AuthMethodConfig::ApiKey(crate::ApiKeyAuthConfig {
                keys: ["alice-key", "bob-key"]
                    .into_iter()
                    .map(|key| crate::ApiKeyEntry::new(key, Vec::<String>::new()))
                    .collect(),
            })],
            mcp_allowlist: None,
        };
    });
    let router = http_router(server, LOCAL_URL);
    let mut alice_id = JsonValue::Null;
    for key in ["alice-key", "bob-key"] {
        let (_, sent) = rpc_as(
            router.clone(),
            "1.0",
            "SendMessage",
            send_params(),
            Some(key),
        )
        .await;
        assert_eq!(
            sent["result"]["task"]["status"]["state"], "TASK_STATE_COMPLETED",
            "{sent}"
        );
        let id = sent["result"]["task"]["id"].clone();
        if key == "alice-key" {
            alice_id = id.clone();
        }
        let (_, listed) = rpc_as(router.clone(), "1.0", "ListTasks", json!({}), Some(key)).await;
        assert_eq!(listed["result"]["totalSize"], 1, "{listed}");
        assert_eq!(listed["result"]["tasks"][0]["id"], id);
    }
    let (_, allowed) = rpc_as(
        router.clone(),
        "1.0",
        "GetTask",
        json!({"id":alice_id}),
        Some("alice-key"),
    )
    .await;
    assert_eq!(allowed["result"]["id"], alice_id);
    for method in ["GetTask", "CancelTask", "SubscribeToTask"] {
        let (_, denied) = rpc_as(
            router.clone(),
            "1.0",
            method,
            json!({"id":alice_id}),
            Some("bob-key"),
        )
        .await;
        assert_eq!(denied["error"]["code"], A2A_TASK_NOT_FOUND, "{denied}");
    }
    let (status, _) = rpc(router, "1.0", "ListTasks", json!({})).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}
