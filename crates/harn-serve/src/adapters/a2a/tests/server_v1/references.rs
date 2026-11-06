use super::*;

#[tokio::test]
async fn task_references_are_authenticated_before_continuation_refusal() {
    let (dir, server) = test_server_with(include_str!("../fixtures/write_task.harn"), |config| {
        config.auth_policy = AuthPolicy {
            methods: vec![AuthMethodConfig::ApiKey(crate::ApiKeyAuthConfig {
                keys: ["alice", "bob"]
                    .into_iter()
                    .map(|key| crate::ApiKeyEntry::new(key, Vec::<String>::new()))
                    .collect(),
            })],
            mcp_allowlist: None,
        };
    });
    let router = http_router(server.clone(), LOCAL_URL);
    let reached = dir.path().join("reached.txt");
    let forbidden = dir.path().join("must-not-execute.txt");
    let mut params = send_params();
    params["message"]["parts"] = json!([{"text":reached}]);
    let (_, initial) = rpc_as(
        router.clone(),
        "1.0",
        "SendMessage",
        params.clone(),
        Some("alice"),
    )
    .await;
    let id = initial["result"]["task"]["id"]
        .as_str()
        .expect("actual task identity");
    assert_eq!(
        initial["result"]["task"]["status"]["state"], "TASK_STATE_COMPLETED",
        "{initial}"
    );
    assert_eq!(
        std::fs::read_to_string(reached).unwrap(),
        "actual execution"
    );
    let before = server.task_json(id);
    for method in ["SendMessage", "SendStreamingMessage"] {
        for (key, reference, expected) in [
            ("alice", "missing-reference", A2A_TASK_NOT_FOUND),
            ("bob", id, A2A_TASK_NOT_FOUND),
            (
                "alice",
                id,
                super::super::super::wire::UNSUPPORTED_OPERATION,
            ),
        ] {
            params["message"]["taskId"] = json!(reference);
            params["message"]["parts"] = json!([{"text":forbidden}]);
            let (_, refused) =
                rpc_as(router.clone(), "1.0", method, params.clone(), Some(key)).await;
            assert_eq!(
                refused["error"]["code"], expected,
                "{method}/{key}/{reference}: {refused}"
            );
            assert_eq!(
                server.tasks.lock().unwrap().len(),
                1,
                "a reference must not allocate another task"
            );
            assert_eq!(
                server.task_json(id),
                before,
                "a refusal must not mutate the known task"
            );
            assert!(!forbidden.exists(), "a refusal must not execute the script");
        }
    }
}
