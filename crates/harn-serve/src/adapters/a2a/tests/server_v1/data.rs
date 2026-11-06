use super::*;

#[tokio::test]
async fn arbitrary_json_data_survives_modern_tasks_and_legacy_projection() {
    let (_dir, server) = echo_server();
    let router = http_router(server, LOCAL_URL);
    for data in [
        json!(2),
        json!([1, "two"]),
        JsonValue::Null,
        json!({"value":2}),
    ] {
        let mut params = send_params();
        params["message"]["parts"] = json!([{"text":"hello"}, {"data":data}]);
        let (_, sent) = rpc(router.clone(), "1.0", "SendMessage", params).await;
        assert_eq!(
            sent["result"]["task"]["status"]["state"], "TASK_STATE_COMPLETED",
            "{sent}"
        );
        assert_upstream("SendMessageResponse", &sent["result"]);
        let task = &sent["result"]["task"];
        assert_eq!(task["history"][0]["parts"][1]["data"], data);
        assert_eq!(task["history"][1]["parts"][0]["text"], "hello");
        let (_, legacy) = rpc(router.clone(), "", "tasks/get", json!({"id":task["id"]})).await;
        assert_legacy_upstream("Task", &legacy["result"]);
        let part = &legacy["result"]["history"][0]["parts"][1];
        if data.is_object() {
            assert_eq!(part["data"], data);
        } else {
            assert_eq!(part["text"], data.to_string());
        }
        let (_, modern) = rpc(router.clone(), "1.0", "GetTask", json!({"id":task["id"]})).await;
        assert_eq!(
            modern["result"]["history"][0]["parts"][1]["data"], data,
            "a legacy projection must not mutate canonical stored data"
        );
    }
}

#[tokio::test]
async fn streamed_json_data_uses_each_subscribers_wire_version() {
    const SOURCE: &str = include_str!("../fixtures/data_reply.harn");
    let (_dir, server) = test_server(SOURCE);
    let router = http_router(server, LOCAL_URL);
    let expected = [json!(2), json!([1, "two"]), JsonValue::Null, json!("hello")];
    for modern in [false, true] {
        let params = if modern {
            send_params()
        } else {
            json!({"message":{"role":"user","parts":[{"kind":"text","text":"hello"}]}})
        };
        let request = harn_vm::jsonrpc::request(
            "data-stream",
            if modern {
                "SendStreamingMessage"
            } else {
                "message/stream"
            },
            params,
        );
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/")
                    .header(A2A_VERSION_HEADER, if modern { "1.0" } else { "0.3" })
                    .body(Body::from(serde_json::to_vec(&request).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let text = String::from_utf8(
            to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        let frames: Vec<JsonValue> = text
            .lines()
            .filter_map(|line| line.strip_prefix("data: "))
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert!(
            !frames.is_empty(),
            "the actual producer must emit stream frames"
        );
        for frame in &frames {
            if modern {
                assert_upstream("StreamResponse", &frame["result"]);
            } else {
                assert_legacy_upstream("SendStreamingMessageSuccessResponse", frame);
            }
        }
        let message = frames
            .iter()
            .find_map(|frame| {
                frame.pointer(if modern {
                    "/result/statusUpdate/status/message"
                } else {
                    "/result/message"
                })
            })
            .expect("actual reply event");
        let parts = message["parts"].as_array().unwrap();
        assert!(message["messageId"].is_string());
        let (_, retrieved) = rpc(
            router.clone(),
            if modern { "1.0" } else { "" },
            if modern { "GetTask" } else { "tasks/get" },
            json!({"id":message["taskId"]}),
        )
        .await;
        assert_eq!(
            retrieved["result"]["history"][1]["messageId"], message["messageId"],
            "stream and retrieval must project the same canonical reply identity"
        );
        assert_eq!(parts.len(), expected.len());
        for (part, expected) in parts.iter().zip(&expected) {
            if modern {
                assert!(
                    part.as_object().unwrap().contains_key("data"),
                    "explicit null is a measured data value"
                );
                assert_eq!(&part["data"], expected);
            } else {
                assert_eq!(part["text"], expected.to_string());
            }
        }
    }
}
