// OAuth admission for the stable HTTP transport: the 401 protected-resource
// challenge, the 403 insufficient-scope step-up, and RFC 8693 token exchange
// for delegated actor chains — together with the mock authorization servers
// those rounds need. Split out of `tests.rs` (#6091); the cases move verbatim.
use super::*;

#[tokio::test(flavor = "current_thread")]
async fn stable_http_401_auth_required_waits_for_oauth_and_retries_tool_call() {
    let _guard = http_mcp_test_guard().await;
    tokio::task::LocalSet::new()
        .run_until(async {
            let (base_url, mut requests, auth_challenged) =
                spawn_auth_required_stable_http_mcp_server().await;
            let handle = stable_http_handle(&base_url).await;
            let server_url = format!("{base_url}/mcp");
            let resource = crate::mcp_auth::canonical_resource_indicator(&server_url).unwrap();
            let session_id =
                crate::agent_sessions::open_or_create(Some("mcp-auth-retry".to_string()));
            let _session_guard = crate::agent_sessions::enter_current_session(session_id.clone());
            let _bridge_guard = CurrentHostBridgeGuard::install();
            let captured_events = install_capturing_agent_sink(&session_id);

            let notifier = tokio::spawn({
                let resource = resource.clone();
                async move {
                    auth_challenged
                        .await
                        .expect("mock server should issue an auth challenge");
                    let token = test_stored_mcp_token(&resource, "fresh-token");
                    crate::mcp_oauth::notify_authorization_completed(&token);
                }
            });

            let result = call_mcp_tool(
                &handle,
                "execute_sql",
                serde_json::json!({"region": "us-west1", "query": "select 1"}),
            )
            .await
            .unwrap();
            notifier.await.expect("auth notifier task should complete");
            assert_eq!(result, serde_json::json!("ok"));

            let discover = recv_recorded_request(&mut requests).await;
            assert_stable_http_request(&discover, "server/discover", None);
            let first_call = recv_recorded_request(&mut requests).await;
            assert_stable_http_request(&first_call, "tools/call", Some("execute_sql"));
            assert!(!first_call.headers.contains_key("authorization"));
            let retry_call = recv_recorded_request(&mut requests).await;
            assert_stable_http_request(&retry_call, "tools/call", Some("execute_sql"));
            assert_eq!(
                retry_call.headers.get("authorization").map(String::as_str),
                Some("Bearer fresh-token")
            );

            let events = captured_events.lock().unwrap().clone();
            assert!(
                events.iter().any(|event| matches!(
                    event,
                    crate::agent_events::AgentEvent::McpAuthRequired {
                        session_id: event_session_id,
                        server,
                        resource: event_resource,
                        scope: Some(scope),
                    } if event_session_id == &session_id
                        && server == "stable-http"
                        && event_resource == &resource
                        && scope == "repo"
                )),
                "expected McpAuthRequired event, got {events:?}"
            );
            crate::agent_events::clear_session_sinks(&session_id);
            handle.disconnect().await.unwrap();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn stable_http_403_insufficient_scope_waits_for_step_up_and_retries_tool_call() {
    let _guard = http_mcp_test_guard().await;
    tokio::task::LocalSet::new()
        .run_until(async {
            let (base_url, mut requests, auth_challenged) =
                spawn_insufficient_scope_stable_http_mcp_server().await;
            let handle = stable_http_handle(&base_url).await;
            let server_url = format!("{base_url}/mcp");
            let resource = crate::mcp_auth::canonical_resource_indicator(&server_url).unwrap();
            let session_id =
                crate::agent_sessions::open_or_create(Some("mcp-scope-stepup".to_string()));
            let _session_guard = crate::agent_sessions::enter_current_session(session_id.clone());
            let _bridge_guard = CurrentHostBridgeGuard::install();
            let captured_events = install_capturing_agent_sink(&session_id);

            let notifier = tokio::spawn({
                let resource = resource.clone();
                async move {
                    auth_challenged
                        .await
                        .expect("mock server should issue an insufficient_scope challenge");
                    let token = test_stored_mcp_token(&resource, "fresh-token");
                    crate::mcp_oauth::notify_authorization_completed(&token);
                }
            });

            let result = call_mcp_tool(
                &handle,
                "execute_sql",
                serde_json::json!({"region": "us-west1", "query": "select 1"}),
            )
            .await
            .unwrap();
            notifier.await.expect("auth notifier task should complete");
            assert_eq!(result, serde_json::json!("ok"));

            let discover = recv_recorded_request(&mut requests).await;
            assert_stable_http_request(&discover, "server/discover", None);
            let first_call = recv_recorded_request(&mut requests).await;
            assert_stable_http_request(&first_call, "tools/call", Some("execute_sql"));
            let retry_call = recv_recorded_request(&mut requests).await;
            assert_stable_http_request(&retry_call, "tools/call", Some("execute_sql"));
            assert_eq!(
                retry_call.headers.get("authorization").map(String::as_str),
                Some("Bearer fresh-token")
            );

            let events = captured_events.lock().unwrap().clone();
            assert!(
                events.iter().any(|event| matches!(
                    event,
                    crate::agent_events::AgentEvent::McpAuthRequired {
                        session_id: event_session_id,
                        server,
                        resource: event_resource,
                        scope: Some(scope),
                    } if event_session_id == &session_id
                        && server == "stable-http"
                        && event_resource == &resource
                        // The step-up event carries the elevated scope from the
                        // insufficient_scope challenge, not just the base scope.
                        && scope == "repo admin"
                )),
                "expected McpAuthRequired step-up event with elevated scope, got {events:?}"
            );
            crate::agent_events::clear_session_sinks(&session_id);
            handle.disconnect().await.unwrap();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn stable_http_401_without_interactive_host_returns_auth_error() {
    let _guard = http_mcp_test_guard().await;
    tokio::task::LocalSet::new()
        .run_until(async {
            crate::llm::clear_current_host_bridge();
            let (base_url, mut requests, _auth_challenged) =
                spawn_auth_required_stable_http_mcp_server().await;
            let handle = stable_http_handle(&base_url).await;
            let session_id =
                crate::agent_sessions::open_or_create(Some("mcp-auth-headless".to_string()));
            let _session_guard = crate::agent_sessions::enter_current_session(session_id);
            let error = call_mcp_tool(
                &handle,
                "execute_sql",
                serde_json::json!({"region": "us-west1", "query": "select 1"}),
            )
            .await
            .expect_err("headless MCP auth challenge should fail clearly");

            match error {
                VmError::CategorizedError { category, message } => {
                    assert_eq!(category, crate::value::ErrorCategory::Auth);
                    assert!(message.contains("stable-http"), "{message}");
                    assert!(message.contains("no interactive host"), "{message}");
                }
                other => panic!("expected categorized auth error, got {other:?}"),
            }

            let discover = recv_recorded_request(&mut requests).await;
            assert_stable_http_request(&discover, "server/discover", None);
            let first_call = recv_recorded_request(&mut requests).await;
            assert_stable_http_request(&first_call, "tools/call", Some("execute_sql"));
            assert!(
                matches!(requests.try_recv(), Err(mpsc::error::TryRecvError::Empty)),
                "headless path should not retry"
            );
            handle.disconnect().await.unwrap();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn stable_http_token_exchange_sends_delegated_bearer_for_actor_chain() {
    let _guard = http_mcp_test_guard().await;
    tokio::task::LocalSet::new()
        .run_until(async {
            let (base_url, mut requests, mut exchanges) =
                spawn_token_exchange_http_mcp_server(true, "Bearer delegated-token").await;
            let spec = token_exchange_http_spec(&base_url);
            let handle = connect_mcp_server_from_spec(&spec)
                .await
                .expect("stable HTTP MCP server should connect");
            let actor_chain =
                crate::actor_chain::ActorChain::new_with_scopes("user:kenneth", ["repo"])
                    .pushed_with_scopes("agent:merge-captain", ["repo"]);
            let session_id = crate::agent_sessions::open_or_create_with_actor_chain(
                Some("mcp-token-exchange".to_string()),
                Some(actor_chain),
            );
            let _session_guard = crate::agent_sessions::enter_current_session(session_id);

            let result = call_mcp_tool(
                &handle,
                "execute_sql",
                serde_json::json!({"region": "us-west1", "query": "select 1"}),
            )
            .await
            .unwrap();
            assert_eq!(result, serde_json::json!("ok"));

            let discover = recv_recorded_request(&mut requests).await;
            assert_stable_http_request(&discover, "server/discover", None);
            assert_eq!(
                discover.headers.get("authorization").map(String::as_str),
                Some("Bearer base-token")
            );
            let tool_call = recv_recorded_request(&mut requests).await;
            assert_stable_http_request(&tool_call, "tools/call", Some("execute_sql"));
            assert_eq!(
                tool_call.headers.get("authorization").map(String::as_str),
                Some("Bearer delegated-token")
            );

            let exchange = recv_token_exchange_form(&mut exchanges).await;
            assert_eq!(
                exchange.get("grant_type").map(String::as_str),
                Some("urn:ietf:params:oauth:grant-type:token-exchange")
            );
            assert_eq!(
                exchange.get("subject_token").map(String::as_str),
                Some("base-token")
            );
            assert_eq!(
                exchange.get("subject_token_type").map(String::as_str),
                Some("urn:ietf:params:oauth:token-type:access_token")
            );
            assert_eq!(
                exchange.get("actor_token").map(String::as_str),
                Some("agent.jwt")
            );
            assert_eq!(
                exchange.get("actor_token_type").map(String::as_str),
                Some("urn:ietf:params:oauth:token-type:jwt")
            );
            assert_eq!(exchange.get("scope").map(String::as_str), Some("repo"));
            let expected_resource = format!("{base_url}/mcp");
            assert_eq!(
                exchange.get("resource").map(String::as_str),
                Some(expected_resource.as_str())
            );
            handle.disconnect().await.unwrap();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn stable_http_token_exchange_unsupported_grant_falls_back_to_plain_bearer() {
    let _guard = http_mcp_test_guard().await;
    tokio::task::LocalSet::new()
        .run_until(async {
            let (base_url, mut requests, mut exchanges) =
                spawn_token_exchange_http_mcp_server(false, "Bearer base-token").await;
            let spec = token_exchange_http_spec(&base_url);
            let handle = connect_mcp_server_from_spec(&spec)
                .await
                .expect("stable HTTP MCP server should connect");
            let actor_chain =
                crate::actor_chain::ActorChain::new("user:kenneth").pushed("agent:merge-captain");
            let session_id = crate::agent_sessions::open_or_create_with_actor_chain(
                Some("mcp-token-exchange-fallback".to_string()),
                Some(actor_chain),
            );
            let _session_guard = crate::agent_sessions::enter_current_session(session_id);

            let result = call_mcp_tool(
                &handle,
                "execute_sql",
                serde_json::json!({"region": "us-west1", "query": "select 1"}),
            )
            .await
            .unwrap();
            assert_eq!(result, serde_json::json!("ok"));

            let _discover = recv_recorded_request(&mut requests).await;
            let tool_call = recv_recorded_request(&mut requests).await;
            assert_eq!(
                tool_call.headers.get("authorization").map(String::as_str),
                Some("Bearer base-token")
            );
            let exchange = recv_token_exchange_form(&mut exchanges).await;
            assert_eq!(
                exchange.get("grant_type").map(String::as_str),
                Some("urn:ietf:params:oauth:grant-type:token-exchange")
            );
            handle.disconnect().await.unwrap();
        })
        .await;
}

fn token_exchange_http_spec(base_url: &str) -> McpServerSpec {
    McpServerSpec {
        name: "token-exchange-http".to_string(),
        transport: McpTransport::Http,
        command: String::new(),
        args: Vec::new(),
        env: BTreeMap::new(),
        cwd: None,
        url: format!("{base_url}/mcp"),
        headers: BTreeMap::new(),
        auth_token: Some("base-token".to_string()),
        token_exchange: Some(crate::mcp_oauth::McpTokenExchangeConfig {
            token_url: Some(format!("{base_url}/token")),
            actor_token: Some("agent.jwt".to_string()),
            ..Default::default()
        }),
        protocol_version: Some(PROTOCOL_VERSION.to_string()),
        proxy_server_name: None,
    }
}

async fn spawn_auth_required_stable_http_mcp_server() -> (
    String,
    mpsc::UnboundedReceiver<RecordedHttpRequest>,
    oneshot::Receiver<()>,
) {
    // A `401 Unauthorized` with a Bearer challenge: no/invalid token.
    spawn_challenge_then_ok_stable_http_mcp_server("401 Unauthorized", r#"Bearer scope="repo""#)
        .await
}

async fn spawn_insufficient_scope_stable_http_mcp_server() -> (
    String,
    mpsc::UnboundedReceiver<RecordedHttpRequest>,
    oneshot::Receiver<()>,
) {
    // A `403 Forbidden` with `error="insufficient_scope"`: a valid token that
    // lacks a required scope. Resolvable by a step-up authorization requesting
    // the elevated `scope` from the challenge.
    spawn_challenge_then_ok_stable_http_mcp_server(
        "403 Forbidden",
        r#"Bearer error="insufficient_scope", scope="repo admin""#,
    )
    .await
}

/// Stable-HTTP MCP mock that answers the first `tools/call` (and any call
/// without a `Bearer fresh-token`) with `status_line` + the given
/// `WWW-Authenticate` `challenge`, then serves `200 OK` once the fresh token
/// is presented. Used to exercise both the `401` and `403 insufficient_scope`
/// step-up authorization paths through one code path.
async fn spawn_challenge_then_ok_stable_http_mcp_server(
    status_line: &'static str,
    challenge: &'static str,
) -> (
    String,
    mpsc::UnboundedReceiver<RecordedHttpRequest>,
    oneshot::Receiver<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (request_tx, request_rx) = mpsc::unbounded_channel();
    let (auth_challenged_tx, auth_challenged_rx) = oneshot::channel();

    tokio::spawn(async move {
        let mut challenged = false;
        let mut auth_challenged_tx = Some(auth_challenged_tx);
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let Ok((_request_line, headers, body)) = read_http_request(&mut stream).await else {
                continue;
            };
            let Ok(request) = serde_json::from_slice::<serde_json::Value>(&body) else {
                continue;
            };
            let _ = request_tx.send(RecordedHttpRequest {
                headers: headers.clone(),
                body: request.clone(),
            });
            let method = request.get("method").and_then(|value| value.as_str());
            if method == Some("tools/call") && !challenged {
                challenged = true;
                if let Some(sender) = auth_challenged_tx.take() {
                    let _ = sender.send(());
                }
                let _ = write_http_json(
                    &mut stream,
                    status_line,
                    &[("WWW-Authenticate", challenge)],
                    serde_json::json!({"error": "authorization required"}),
                )
                .await;
                continue;
            }
            if method == Some("tools/call")
                && headers.get("authorization").map(String::as_str) != Some("Bearer fresh-token")
            {
                let _ = write_http_json(
                    &mut stream,
                    status_line,
                    &[("WWW-Authenticate", challenge)],
                    serde_json::json!({"error": "authorization required"}),
                )
                .await;
                continue;
            }
            let response = stable_http_response(&request, method);
            let _ = write_http_json(&mut stream, "200 OK", &[], response).await;
        }
    });

    (format!("http://{addr}"), request_rx, auth_challenged_rx)
}

async fn spawn_token_exchange_http_mcp_server(
    exchange_supported: bool,
    expected_tool_authorization: &'static str,
) -> (
    String,
    mpsc::UnboundedReceiver<RecordedHttpRequest>,
    mpsc::UnboundedReceiver<BTreeMap<String, String>>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (request_tx, request_rx) = mpsc::unbounded_channel();
    let (exchange_tx, exchange_rx) = mpsc::unbounded_channel();

    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let Ok((request_line, headers, body)) = read_http_request(&mut stream).await else {
                continue;
            };
            if request_line.starts_with("POST /token ") {
                let form = url::form_urlencoded::parse(&body)
                    .into_owned()
                    .collect::<BTreeMap<_, _>>();
                let _ = exchange_tx.send(form);
                if exchange_supported {
                    let body = serde_json::json!({
                        "access_token": "delegated-token",
                        "issued_token_type": "urn:ietf:params:oauth:token-type:access_token",
                        "token_type": "Bearer",
                        "expires_in": 300,
                    });
                    let _ = write_http_json(&mut stream, "200 OK", &[], body).await;
                } else {
                    let body = serde_json::json!({"error": "unsupported_grant_type"});
                    let _ = write_http_json(&mut stream, "400 Bad Request", &[], body).await;
                }
                continue;
            }

            let Ok(request) = serde_json::from_slice::<serde_json::Value>(&body) else {
                let _ = write_http_empty(&mut stream, "400 Bad Request").await;
                continue;
            };
            let _ = request_tx.send(RecordedHttpRequest {
                headers: headers.clone(),
                body: request.clone(),
            });
            let method = request.get("method").and_then(|value| value.as_str());
            if method == Some("tools/call")
                && headers.get("authorization").map(String::as_str)
                    != Some(expected_tool_authorization)
            {
                let _ = write_http_json(
                    &mut stream,
                    "401 Unauthorized",
                    &[("WWW-Authenticate", r#"Bearer scope="repo""#)],
                    serde_json::json!({"error": "authorization required"}),
                )
                .await;
                continue;
            }
            let response = stable_http_response(&request, method);
            let _ = write_http_json(&mut stream, "200 OK", &[], response).await;
        }
    });

    (format!("http://{addr}"), request_rx, exchange_rx)
}

async fn recv_token_exchange_form(
    exchanges: &mut mpsc::UnboundedReceiver<BTreeMap<String, String>>,
) -> BTreeMap<String, String> {
    tokio::time::timeout(MCP_TIMEOUT, exchanges.recv())
        .await
        .expect("timed out waiting for token exchange request")
        .expect("mock server closed before recording token exchange")
}

fn test_stored_mcp_token(resource: &str, access_token: &str) -> crate::mcp_oauth::StoredMcpToken {
    crate::mcp_oauth::StoredMcpToken {
        access_token: access_token.to_string(),
        refresh_token: None,
        expires_at_unix: None,
        token_endpoint: "https://auth.example/token".to_string(),
        client_id: "test-client".to_string(),
        client_secret: None,
        token_endpoint_auth_method: "none".to_string(),
        issuer: "https://auth.example".to_string(),
        resource: resource.to_string(),
        scopes: Some("repo".to_string()),
        token_response_extra: None,
    }
}
