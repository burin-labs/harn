// Fixture lifecycle over the stable HTTP transport.
//
// A harness installs capability fixtures *after* `mcp_connect` returns, so the
// client must read fixture state when an inbound request arrives rather than
// capture it at connect time. Harn #6071 was the failure mode this guards: the
// pre-rmcp client kept a long-lived GET/SSE task and restarted it on every
// fixture install, which could strand a server-to-client `elicitation/create`
// on the abandoned connection until the one-minute MCP deadline.

use super::*;

/// Build a capability fixture state that answers `mcp.elicit` with an accept.
fn accepting_elicit_fixtures(answer: &str) -> Arc<crate::harness::CapabilityFixtureState> {
    let fixtures = Arc::new(crate::harness::CapabilityFixtureState::default());
    fixtures.respond(
        "mcp",
        "elicit",
        Ok(json_to_vm_value(&serde_json::json!({
            "action": "accept",
            "content": {"answer": answer},
        }))),
        None,
        true,
    );
    fixtures
}

#[tokio::test(flavor = "current_thread")]
async fn fixtures_installed_after_connect_answer_inbound_elicitation() {
    let _guard = http_mcp_test_guard().await;
    tokio::task::LocalSet::new()
        .run_until(async {
            let (base_url, mut requests) = spawn_stable_http_mcp_server().await;
            let handle = stable_http_handle(&base_url).await;

            // The harness installs fixtures only after the client is live.
            handle
                .set_capability_fixtures(accepting_elicit_fixtures("from-fixture"))
                .await;
            install_sampling_mock().await;

            let result = call_mcp_tool(
                &handle,
                "needs_input",
                serde_json::json!({"prompt": "continue"}),
            )
            .await
            .unwrap();
            assert_eq!(result, serde_json::json!("done"));

            let _discover = recv_recorded_request(&mut requests).await;
            let _first_call = recv_recorded_request(&mut requests).await;
            let retry_call = recv_recorded_request(&mut requests).await;
            let responses = &retry_call.body["params"]["inputResponses"];
            assert_eq!(
                responses["elicitation"]["action"],
                serde_json::json!("accept"),
                "a fixture installed after connect must answer the inbound elicitation"
            );
            assert_eq!(
                responses["elicitation"]["content"]["answer"],
                serde_json::json!("from-fixture")
            );

            clear_sampling_mock().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn reinstalling_fixtures_replaces_the_answer_without_reconnecting() {
    let _guard = http_mcp_test_guard().await;
    tokio::task::LocalSet::new()
        .run_until(async {
            let (base_url, mut requests) = spawn_stable_http_mcp_server().await;
            let handle = stable_http_handle(&base_url).await;
            install_sampling_mock().await;

            handle
                .set_capability_fixtures(accepting_elicit_fixtures("first"))
                .await;
            call_mcp_tool(
                &handle,
                "needs_input",
                serde_json::json!({"prompt": "continue"}),
            )
            .await
            .unwrap();
            let _discover = recv_recorded_request(&mut requests).await;
            let _first_call = recv_recorded_request(&mut requests).await;
            let first_retry = recv_recorded_request(&mut requests).await;
            assert_eq!(
                first_retry.body["params"]["inputResponses"]["elicitation"]["content"]["answer"],
                serde_json::json!("first")
            );

            // A second install must be observed by the same live client. The
            // sampling mock is single-shot, so re-arm it for the second round.
            install_sampling_mock().await;
            handle
                .set_capability_fixtures(accepting_elicit_fixtures("second"))
                .await;
            call_mcp_tool(
                &handle,
                "needs_input",
                serde_json::json!({"prompt": "continue"}),
            )
            .await
            .unwrap();
            let _second_call = recv_recorded_request(&mut requests).await;
            let second_retry = recv_recorded_request(&mut requests).await;
            assert_eq!(
                second_retry.body["params"]["inputResponses"]["elicitation"]["content"]["answer"],
                serde_json::json!("second"),
                "reinstalling fixtures must take effect on the existing client"
            );

            clear_sampling_mock().await;
        })
        .await;
}
