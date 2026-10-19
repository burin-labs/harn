use super::*;

#[tokio::test]
async fn private_inputs_reach_the_served_pipeline_without_entering_events_or_forks() {
    let (_directory, server) = pipeline_server(
        r"
fn main(harness: Harness) {
  harness.stdio.println(json_stringify({reached: true, inputs: harness.secrets.inputs()}))
}
",
    );
    let state = server.state.clone();
    let mut events = state.events_tx.subscribe();
    let app = api_router(server.state);
    let response = post(&app, "/v1/sessions", json!({})).await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let session = read_json(response).await;
    let id = session["id"].as_str().unwrap();
    let path = format!("/v1/sessions/{id}/inputs");
    let response = post(
        &app,
        &path,
        json!({
            "kind":"put", "spec":{"name":"SERVICE_KEY"}, "value":"private-api-canary"
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(read_json(response).await["input"]["name"], "SERVICE_KEY");
    let listed = read_json(post(&app, &path, json!({"kind":"list"})).await).await;
    assert_eq!(listed["inputs"][0]["name"], "SERVICE_KEY");
    assert!(listed["inputs"][0].get("value").is_none());
    let response = post(&app, &path, json!({"kind":"sync", "snapshot":{
        "sourceId":"ide-window", "revision":1,
        "inputs":[{"spec":{"name":"WINDOW_KEY"},"value":"private-window-canary"}], "redactions":[]
    }})).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        read_json(response).await["inputs"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let response = post(
        &app,
        &format!("/v1/sessions/{id}/tasks"),
        json!({"input":"Inspect names."}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let mut output = String::new();
    loop {
        let event = events.recv().await.unwrap();
        assert!(!event.payload.to_string().contains("private-api-canary"));
        assert!(!event.payload.to_string().contains("private-window-canary"));
        output.push_str(&event.payload.to_string());
        assert_ne!(event.event, "task.failed", "{}", event.payload);
        if event.event == "task.completed" {
            break;
        }
    }
    assert!(output.contains("SERVICE_KEY"));
    assert!(output.contains("WINDOW_KEY"));
    assert!(output.contains("reached"));
    let messages = read_json(get(&app, &format!("/v1/sessions/{id}/messages")).await).await;
    assert!(!messages.to_string().contains("private-api-canary"));
    let response = post(
        &app,
        &path,
        json!({"kind":"sync", "snapshot":{
            "sourceId":"ide-window", "revision":2, "inputs":[], "redactions":[]
        }}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let remaining = read_json(response).await;
    assert_eq!(remaining["inputs"].as_array().unwrap().len(), 1);
    assert_eq!(remaining["inputs"][0]["name"], "SERVICE_KEY");
    let fork = post(&app, &format!("/v1/sessions/{id}/fork"), json!({})).await;
    assert_eq!(fork.status(), StatusCode::CREATED);
    let fork = read_json(fork).await;
    let fork_id = fork["id"].as_str().unwrap();
    let listed = read_json(
        post(
            &app,
            &format!("/v1/sessions/{fork_id}/inputs"),
            json!({"kind":"list"}),
        )
        .await,
    )
    .await;
    assert!(listed["inputs"].as_array().unwrap().is_empty());
    let closed = post(&app, &format!("/v1/sessions/{id}/close"), json!({})).await;
    assert_eq!(closed.status(), StatusCode::OK);
    assert_ne!(
        post(&app, &path, json!({"kind":"list"})).await.status(),
        StatusCode::OK
    );
}
