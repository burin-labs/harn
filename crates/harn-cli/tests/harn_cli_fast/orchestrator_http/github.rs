use super::support::*;

#[tokio::test(flavor = "multi_thread")]
async fn github_ingress_runs_matching_personas_and_enforces_route_selectors() {
    let temp = TempDir::new().unwrap();
    let mut manifest = base_manifest(None).replace("issues.opened", "pull_request.opened");
    manifest.push_str(
        r#"
[[triggers]]
id = "other-route"
kind = "webhook"
provider = "github"
match = { events = ["pull_request.opened"] }
handler = "persona://first"
secrets = { signing_secret = "github/webhook-secret" }

[[personas]]
name = "first"
description = "Handles opened pull requests."
entry_workflow = "workflow.harn#run"
tools = ["github"]
autonomy = "suggest"
receipts = "required"
triggers = ["github.pull_request.opened"]

[[personas]]
name = "second"
description = "Also handles opened pull requests."
entry_workflow = "workflow.harn#run"
tools = ["github"]
autonomy = "suggest"
receipts = "required"
triggers = ["github.pull_request.opened"]

[[personas]]
name = "closed"
description = "Handles closed pull requests."
entry_workflow = "workflow.harn#run"
tools = ["github"]
autonomy = "suggest"
receipts = "required"
triggers = ["github.pull_request.closed"]
"#,
    );
    write_file(temp.path(), "harn.toml", &manifest);
    write_file(temp.path(), "lib.harn", handler_module());
    write_file(
        temp.path(),
        "github_connector.harn",
        github_connector_module(),
    );
    write_file(temp.path(), "workflow.harn",
        "pub pipeline run(_harness: Harness, event) { return {executed: true, kind: event.kind} }\n");
    let secret = "persona-ingress-secret";
    let _envs = lock_env_with(&[
        ("HARN_SECRET_PROVIDERS", "env"),
        ("HARN_SECRET_GITHUB_WEBHOOK_SECRET", secret),
        ("HARN_ORCHESTRATOR_API_KEYS", "persona-reload-key"),
    ])
    .await;
    let harness = start_harness(&temp).await;
    let log = harness.event_log();
    for (index, (route, action, delivery, number)) in [
        ("github-new-issue", "opened", "shared-delivery", 1),
        ("other-route", "opened", "shared-delivery", 1),
        ("github-new-issue", "closed", "closed-delivery", 2),
        ("github-new-issue", "opened", "after-rollback", 3),
        ("github-new-issue", "opened", "after-reload", 4),
        ("github-new-issue", "reopened", "reopened-delivery", 5),
    ]
    .into_iter()
    .enumerate()
    {
        if index == 3 || index == 4 {
            let next = if index == 3 {
                manifest
                    .replace("github.pull_request.opened", "github.pull_request.edited")
                    .replace(
                        "kind = \"webhook\"",
                        "kind = \"webhook\"\npath = \"/collision\"",
                    )
            } else {
                manifest.replace("github.pull_request.opened", "github.pull_request.reopened")
            };
            write_file(temp.path(), "harn.toml", &next);
            let reload = reqwest::Client::new()
                .post(format!("{}/admin/reload", harness.listener_url()))
                .bearer_auth("persona-reload-key")
                .json(&serde_json::json!({"source": "persona_test"}))
                .send()
                .await
                .unwrap();
            if index == 3 {
                assert_status(reload, StatusCode::INTERNAL_SERVER_ERROR).await;
                let transitions = read_topic_events(&log, "triggers.lifecycle").await;
                assert!(
                    transitions.iter().any(|(_, event)| {
                        event.kind == "state_transition"
                            && event.payload["id"] == "persona.first.github.pull_request.edited"
                            && event.payload["to_state"] == "active"
                    }),
                    "the failed reload must reach binding installation before rollback"
                );
            } else {
                assert_eq!(reload.status(), StatusCode::OK);
                let body: JsonValue = reload.json().await.unwrap();
                assert_eq!(
                    body["summary"]["removed"],
                    serde_json::json!([
                        "persona.first.github.pull_request.opened",
                        "persona.second.github.pull_request.opened",
                    ])
                );
                assert_eq!(
                    body["summary"]["added"],
                    serde_json::json!([
                        "persona.first.github.pull_request.reopened",
                        "persona.second.github.pull_request.reopened",
                    ])
                );
            }
        }
        let before = log
            .latest(&Topic::new(harn_vm::TRIGGER_INBOX_ENVELOPES_TOPIC).unwrap())
            .await
            .unwrap()
            .unwrap_or(0);
        let body = serde_json::to_vec(&serde_json::json!({
            "action": action,
            "number": number,
            "pull_request": {"number": number},
            "repository": {"full_name": "burin-labs/harn"},
        }))
        .unwrap();
        let mut headers = github_headers(secret, &body, None);
        headers.insert("X-GitHub-Event", HeaderValue::from_static("pull_request"));
        headers.insert(
            "X-GitHub-Delivery",
            HeaderValue::from_str(delivery).unwrap(),
        );
        let response = reqwest::Client::new()
            .post(format!("{}/triggers/{route}", harness.listener_url()))
            .headers(headers)
            .body(body)
            .send()
            .await
            .unwrap();
        assert_status(response, StatusCode::OK).await;
        let completed = await_topic_event(&log, "orchestrator.lifecycle", |event| {
            event.kind == "pump_dispatch_completed"
                && event.payload["topic"] == harn_vm::TRIGGER_INBOX_ENVELOPES_TOPIC
                && event.payload["event_log_id"]
                    .as_u64()
                    .is_some_and(|id| id > before)
        })
        .await;
        assert_eq!(completed.payload["status"], "completed", "{completed:?}");
    }
    shutdown(harness).await;
    let snapshot: JsonValue = serde_json::from_str(&state_snapshot(&temp)).unwrap();
    let bindings = snapshot["triggers"].as_array().unwrap();
    assert_eq!(bindings.len(), 5);
    let subscriber = bindings
        .iter()
        .find(|binding| binding["id"] == "persona.first.github.pull_request.reopened")
        .expect("status must include the active persona subscription");
    assert_eq!(subscriber["handler"], "persona");
    assert_eq!(subscriber["dispatched"], 1);
    assert_eq!(subscriber["in_flight"], 0);
    let outbox = read_topic_events(&log, "trigger.outbox").await;
    let mut duplicates = 0;
    let mut executed = 0;
    for (_, event) in outbox.iter().filter(|(_, event)| {
        event.kind == "dispatch_succeeded" && event.payload["handler_kind"] == "persona"
    }) {
        if event.payload["result"]["status"] == "duplicate" {
            assert_eq!(event.payload["trigger_id"], "other-route");
            duplicates += 1;
        } else {
            assert_eq!(event.payload["result"]["status"], "completed");
            assert_eq!(
                event.payload["result"]["result"]["executed"], true,
                "{}",
                event.payload
            );
            assert_eq!(event.payload["result"]["result"]["kind"], "pull_request");
            executed += 1;
        }
    }
    assert_eq!(duplicates, 1);
    assert_eq!(executed, 7);
    let mut dispatched = outbox
        .iter()
        .filter(|(_, event)| event.kind == "dispatch_succeeded")
        .map(|(_, event)| event.payload["trigger_id"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    dispatched.sort();
    assert_eq!(
        dispatched,
        vec![
            "github-new-issue",
            "github-new-issue",
            "github-new-issue",
            "other-route",
            "persona.closed.github.pull_request.closed",
            "persona.first.github.pull_request.opened",
            "persona.first.github.pull_request.opened",
            "persona.first.github.pull_request.reopened",
            "persona.second.github.pull_request.opened",
            "persona.second.github.pull_request.opened",
            "persona.second.github.pull_request.reopened",
        ]
    );
    let completed = read_topic_events(&log, "persona.runtime.events").await;
    assert_eq!(
        completed
            .iter()
            .filter(|(_, event)| event.kind == "persona.run.completed")
            .count(),
        7
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn github_webhook_delivery_is_accepted_and_persisted() {
    let temp = TempDir::new().unwrap();
    write_file(temp.path(), "harn.toml", &base_manifest(None));
    write_file(temp.path(), "lib.harn", handler_module());
    write_file(
        temp.path(),
        "github_connector.harn",
        github_connector_module(),
    );

    let secret = "integration-test-secret";
    let _envs = lock_env_with(&[
        ("HARN_SECRET_PROVIDERS", "env"),
        ("HARN_SECRET_GITHUB_WEBHOOK_SECRET", secret),
        ("RUST_LOG", "info"),
    ])
    .await;
    let harness = start_harness(&temp).await;
    let base_url = harness.listener_url().to_string();

    let health = reqwest::get(format!("{base_url}/health")).await.unwrap();
    assert_status(health, StatusCode::OK).await;

    let body = br#"{"action":"opened","issue":{"number":1}}"#;
    let response = reqwest::Client::new()
        .post(format!("{base_url}/triggers/github-new-issue"))
        .headers(github_headers(secret, body, None))
        .body(body.to_vec())
        .send()
        .await
        .unwrap();
    assert_status(response, StatusCode::OK).await;

    await_pump_dispatch_completed(&harness).await;
    shutdown(harness).await;

    let snapshot = state_snapshot(&temp);
    assert!(
        snapshot.contains("\"status\": \"stopped\""),
        "snapshot={snapshot}"
    );
    assert!(snapshot.contains("\"received\": 1"), "snapshot={snapshot}");
    assert!(
        snapshot.contains("\"dispatched\": 1"),
        "snapshot={snapshot}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn github_provider_prefers_configured_harn_connector_over_deprecated_rust_default() {
    let temp = TempDir::new().unwrap();
    let marker_path = temp.path().join("github-override-handler.txt");
    write_file(
        temp.path(),
        "harn.toml",
        &github_harn_override_manifest(None),
    );
    write_file(
        temp.path(),
        "lib.harn",
        &github_marker_handler_module(&marker_path),
    );
    write_file(
        temp.path(),
        "github_connector.harn",
        github_override_connector_module(),
    );

    let _envs = lock_env_with(&[
        ("HARN_SECRET_PROVIDERS", "env"),
        ("HARN_SECRET_GITHUB_WEBHOOK_SECRET", "override-secret"),
    ])
    .await;
    let harness = start_harness(&temp).await;
    let base_url = harness.listener_url().to_string();

    let body = serde_json::to_vec(&serde_json::json!({
        "id": "evt-gh-override-1",
        "action": "opened",
        "issue": {"number": 42, "title": "Harn connector override"}
    }))
    .unwrap();
    let response = reqwest::Client::new()
        .post(format!("{base_url}/triggers/github-new-issue"))
        .header(CONTENT_TYPE, "application/json")
        .header("X-GitHub-Event", "issues")
        .body(body)
        .send()
        .await
        .unwrap();
    assert_status(response, StatusCode::OK).await;

    await_pump_dispatch_completed(&harness).await;
    let marker = fs::read_to_string(&marker_path).unwrap();
    assert_eq!(marker, "issues");

    let event_log = harness.event_log();
    shutdown(harness).await;

    let lifecycle = read_topic_events(&event_log, "connectors.github.override").await;
    let lifecycle_kinds: Vec<_> = lifecycle
        .iter()
        .map(|(_, event)| event.kind.as_str())
        .collect();
    assert_eq!(lifecycle_kinds, vec!["init", "normalize"]);
}
