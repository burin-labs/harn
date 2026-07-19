use super::*;
/// End-to-end ACP slash-command flow: a Zed-style client receives the
/// `available_commands_update` notification immediately after
/// `session/new`, then invokes one of the advertised commands and
/// observes a successful round-trip with the named pipeline executed.
/// Locks the wire shape required by the ACP spec
/// (<https://agentclientprotocol.com/protocol/slash-commands>).
#[tokio::test(flavor = "current_thread")]
async fn acp_advertises_and_dispatches_slash_commands() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let pipeline_path = dir.path().join("commands.harn");
            std::fs::write(
                &pipeline_path,
                "@command(name: \"review\", description: \"Review the diff\", \
                     hint: \"focus area\")\n\
                     pipeline review_branch(task) {\n  \
                       __io_println(\"REVIEW:\" + prompt)\n}\n\n\
                     pipeline default(task) {\n  \
                       __io_println(\"DEFAULT:\" + prompt)\n}\n",
            )
            .expect("write pipeline");

            let (request_tx, request_rx) = mpsc::unbounded_channel();
            let (response_tx, mut response_rx) = mpsc::unbounded_channel();
            let server = tokio::task::spawn_local(super::run_acp_channel_server(
                AcpServerConfig::for_pipeline(pipeline_path.to_string_lossy()),
                request_rx,
                response_tx,
            ));

            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "session/new",
                    "params": {"cwd": dir.path()},
                }))
                .expect("send session/new");
            let created = recv_json(&mut response_rx).await;
            let session_id = created["result"]["sessionId"]
                .as_str()
                .expect("session id")
                .to_string();

            let advertised = recv_json(&mut response_rx).await;
            assert_eq!(advertised["method"], "session/update");
            assert_eq!(advertised["params"]["sessionId"], session_id);
            assert_eq!(
                advertised["params"]["update"]["sessionUpdate"],
                "available_commands_update"
            );
            let commands = advertised["params"]["update"]["availableCommands"]
                .as_array()
                .expect("availableCommands array");
            assert_eq!(commands.len(), 1);
            assert_eq!(commands[0]["name"], "review");
            assert_eq!(commands[0]["description"], "Review the diff");
            assert_eq!(commands[0]["input"]["hint"], "focus area");

            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "session/prompt",
                    "params": {
                        "sessionId": session_id,
                        "prompt": [{"type": "text", "text": "/review src/lib.rs"}],
                    },
                }))
                .expect("send session/prompt");

            let mut saw_review_chunk = false;
            let mut saw_completed = false;
            for _ in 0..32 {
                let message = recv_json(&mut response_rx).await;
                if message["method"] == "host/capabilities" {
                    request_tx
                        .send(serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": message["id"].clone(),
                            "result": {},
                        }))
                        .expect("send host capabilities response");
                    continue;
                }
                if message["method"] == "session/update"
                    && message["params"]["update"]["sessionUpdate"] == "agent_message_chunk"
                {
                    let text = message["params"]["update"]["content"]["text"]
                        .as_str()
                        .unwrap_or_default();
                    if text.contains("REVIEW:src/lib.rs") {
                        saw_review_chunk = true;
                    }
                    assert!(
                        !text.contains("DEFAULT:"),
                        "default pipeline must not run when slash command dispatches"
                    );
                }
                if message["id"] == 2 {
                    assert_eq!(message["result"]["stopReason"], "end_turn");
                    saw_completed = true;
                    break;
                }
            }
            assert!(saw_review_chunk, "named pipeline should run for /review");
            assert!(saw_completed, "prompt should finish successfully");

            drop(request_tx);
            server.await.expect("ACP channel server task");
        })
        .await;
}

/// Unknown slash invocations (i.e. `/typo args` when `typo` isn't
/// advertised) must not be re-routed — the original prompt text
/// flows through to the default pipeline so it can decide how to
/// handle the literal slash.
#[tokio::test(flavor = "current_thread")]
async fn acp_unknown_slash_invocation_falls_through_to_default_pipeline() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let pipeline_path = dir.path().join("fallthrough.harn");
            std::fs::write(
                &pipeline_path,
                "@command(name: \"known\", description: \"known\")\n\
                     pipeline known(task) { __io_println(\"KNOWN\") }\n\n\
                     pipeline default(task) { __io_println(\"DEFAULT:\" + prompt) }\n",
            )
            .expect("write pipeline");

            let (request_tx, request_rx) = mpsc::unbounded_channel();
            let (response_tx, mut response_rx) = mpsc::unbounded_channel();
            let server = tokio::task::spawn_local(super::run_acp_channel_server(
                AcpServerConfig::for_pipeline(pipeline_path.to_string_lossy()),
                request_rx,
                response_tx,
            ));

            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "session/new",
                    "params": {"cwd": dir.path()},
                }))
                .expect("send session/new");
            let created = recv_json(&mut response_rx).await;
            let session_id = created["result"]["sessionId"]
                .as_str()
                .expect("session id")
                .to_string();
            let _advertised = recv_json(&mut response_rx).await;

            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "session/prompt",
                    "params": {
                        "sessionId": session_id,
                        "prompt": [{"type": "text", "text": "/typo and friends"}],
                    },
                }))
                .expect("send session/prompt");

            let mut saw_default_with_full_text = false;
            for _ in 0..32 {
                let message = recv_json(&mut response_rx).await;
                if message["method"] == "host/capabilities" {
                    request_tx
                        .send(serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": message["id"].clone(),
                            "result": {},
                        }))
                        .expect("send host capabilities response");
                    continue;
                }
                if message["method"] == "session/update"
                    && message["params"]["update"]["sessionUpdate"] == "agent_message_chunk"
                {
                    let text = message["params"]["update"]["content"]["text"]
                        .as_str()
                        .unwrap_or_default();
                    if text.contains("DEFAULT:/typo and friends") {
                        saw_default_with_full_text = true;
                    }
                }
                if message["id"] == 2 {
                    assert_eq!(message["result"]["stopReason"], "end_turn");
                    break;
                }
            }
            assert!(
                saw_default_with_full_text,
                "default pipeline should receive the full original prompt text"
            );

            drop(request_tx);
            server.await.expect("ACP channel server task");
        })
        .await;
}

/// Inline-prompt mode (no `--pipeline`) has no surface for
/// `@command`-tagged pipelines. A leading slash is unambiguously a
/// user error there; surface a clear diagnostic instead of letting
/// the compile-time `pipeline main() { /foo args }` error leak out.
#[tokio::test(flavor = "current_thread")]
async fn acp_inline_mode_rejects_slash_invocations_with_friendly_error() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut server = AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx));

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "session/new",
            "params": {"cwd": "."},
        }))
        .await;
    let created = recv_json(&mut rx).await;
    let session_id = created["result"]["sessionId"]
        .as_str()
        .expect("session id")
        .to_string();

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "session/prompt",
            "params": {
                "sessionId": session_id,
                "prompt": [{"type": "text", "text": "/foo args"}],
            },
        }))
        .await;

    // The friendly diagnostic is a terminal failure, so it arrives as exactly
    // one typed JSON-RPC error and never as an assistant `agent_message_chunk`.
    let error = recv_json(&mut rx).await;
    assert_eq!(
        error["method"],
        serde_json::Value::Null,
        "no session/update precedes the error"
    );
    assert_eq!(error["id"], 2);
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("Slash commands require `--pipeline"),
        "expected friendly inline-mode error message, got: {error}"
    );
    assert_eq!(
        error["error"]["data"]["schema"],
        ACP_PROMPT_ERROR_DATA_SCHEMA
    );
    assert_eq!(error["error"]["data"]["terminalClass"], "generic_throw");
}

/// Hot-reload: when the pipeline source changes between prompts, the
/// next prompt re-emits `available_commands_update` with the fresh
/// command set. When the source is unchanged, no duplicate update is
/// emitted (idempotent advertise).
#[tokio::test(flavor = "current_thread")]
async fn acp_reemits_available_commands_on_pipeline_hot_reload() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let pipeline_path = dir.path().join("hot.harn");
            std::fs::write(
                &pipeline_path,
                "@command(name: \"alpha\", description: \"first\")\n\
                     pipeline alpha(task) { __io_println(\"alpha\") }\n",
            )
            .expect("write initial pipeline");

            let (request_tx, request_rx) = mpsc::unbounded_channel();
            let (response_tx, mut response_rx) = mpsc::unbounded_channel();
            let server = tokio::task::spawn_local(super::run_acp_channel_server(
                AcpServerConfig::for_pipeline(pipeline_path.to_string_lossy()),
                request_rx,
                response_tx,
            ));

            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "session/new",
                    "params": {"cwd": dir.path()},
                }))
                .expect("send session/new");
            let created = recv_json(&mut response_rx).await;
            let session_id = created["result"]["sessionId"]
                .as_str()
                .expect("session id")
                .to_string();
            let initial = recv_json(&mut response_rx).await;
            let initial_commands = initial["params"]["update"]["availableCommands"]
                .as_array()
                .expect("availableCommands array");
            assert_eq!(initial_commands.len(), 1);
            assert_eq!(initial_commands[0]["name"], "alpha");

            std::fs::write(
                &pipeline_path,
                "@command(name: \"alpha\", description: \"first\")\n\
                     pipeline alpha(task) { __io_println(\"alpha\") }\n\n\
                     @command(name: \"beta\", description: \"second\")\n\
                     pipeline beta(task) { __io_println(\"beta\") }\n",
            )
            .expect("rewrite pipeline");

            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "session/prompt",
                    "params": {
                        "sessionId": session_id,
                        "prompt": [{"type": "text", "text": "/beta now"}],
                    },
                }))
                .expect("send session/prompt");

            let mut saw_refreshed_advertise = false;
            let mut saw_beta_chunk = false;
            for _ in 0..32 {
                let message = recv_json(&mut response_rx).await;
                if message["method"] == "host/capabilities" {
                    request_tx
                        .send(serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": message["id"].clone(),
                            "result": {},
                        }))
                        .expect("send host capabilities response");
                    continue;
                }
                if message["method"] == "session/update"
                    && message["params"]["update"]["sessionUpdate"] == "available_commands_update"
                {
                    let names: Vec<String> = message["params"]["update"]["availableCommands"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|c| c["name"].as_str().unwrap().to_string())
                        .collect();
                    assert_eq!(names, vec!["alpha".to_string(), "beta".to_string()]);
                    saw_refreshed_advertise = true;
                }
                if message["method"] == "session/update"
                    && message["params"]["update"]["sessionUpdate"] == "agent_message_chunk"
                    && message["params"]["update"]["content"]["text"]
                        .as_str()
                        .unwrap_or_default()
                        .contains("beta")
                {
                    saw_beta_chunk = true;
                }
                if message["id"] == 2 {
                    assert_eq!(message["result"]["stopReason"], "end_turn");
                    break;
                }
            }
            assert!(
                saw_refreshed_advertise,
                "expected fresh available_commands_update after source change"
            );
            assert!(
                saw_beta_chunk,
                "the newly added /beta command should dispatch"
            );

            drop(request_tx);
            server.await.expect("ACP channel server task");
        })
        .await;
}

/// `session/prompt` returns the canonical ACP `stopReason` rather
/// than Harn's internal "completed" / "cancelled" pair. This drives
/// each branch of the mapping in `agent_session_host::canonical_acp_stop_reason`
/// through a real ACP roundtrip with `provider: "mock"` so the
/// adapter and the agent loop's finalize stay aligned with the
/// canonical enum at <https://agentclientprotocol.com/protocol/prompt-turn>.
async fn run_acp_agent_loop_prompt(prompt_body: &str) -> serde_json::Value {
    let (request_tx, mut response_rx, server, session_id) = start_acp_channel_session().await;

    // `agent_loop` requires the LLM/network capability ceiling.
    // The default `ask` mode clamps to read-only; switch to `code`
    // (`ActAuto` autonomy tier) so the test can exercise the loop.
    request_tx
        .send(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "session/set_mode",
            "params": {"sessionId": session_id, "modeId": "code"},
        }))
        .expect("send session/set_mode");
    let _ack = recv_json(&mut response_rx).await;
    let _notification = recv_json(&mut response_rx).await;

    request_tx
        .send(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "session/prompt",
            "params": {
                "sessionId": session_id,
                "prompt": [{"type": "text", "text": prompt_body}],
            },
        }))
        .expect("send session/prompt");

    let mut stop_reason = serde_json::Value::Null;
    for _ in 0..64 {
        let message = recv_json(&mut response_rx).await;
        if message["method"] == "host/capabilities" {
            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": message["id"].clone(),
                    "result": {},
                }))
                .expect("send host capabilities response");
            continue;
        }
        if message["id"] == 3 {
            stop_reason = message["result"]["stopReason"].clone();
            break;
        }
    }
    drop(request_tx);
    server.await.expect("ACP channel server task");
    stop_reason
}

#[tokio::test(flavor = "current_thread")]
async fn acp_session_prompt_reports_end_turn_when_loop_finishes_naturally() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let body = "llm_mock_clear()\n\
                            llm_mock({text: \"all done\"})\n\
                            agent_loop(\"hello\", nil, {provider: \"mock\"})";
            let stop_reason = run_acp_agent_loop_prompt(body).await;
            assert_eq!(stop_reason, "end_turn");
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn acp_session_prompt_reports_max_tokens_from_provider_signal() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let body = "llm_mock_clear()\n\
                            llm_mock({text: \"truncated\", stop_reason: \"max_tokens\"})\n\
                            agent_loop(\"hello\", nil, {provider: \"mock\"})";
            let stop_reason = run_acp_agent_loop_prompt(body).await;
            assert_eq!(stop_reason, "max_tokens");
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn acp_session_prompt_reports_refusal_from_provider_signal() {
    let local = tokio::task::LocalSet::new();
    local
            .run_until(async {
                let body = "llm_mock_clear()\n\
                            llm_mock({text: \"I cannot assist with that.\", stop_reason: \"refusal\"})\n\
                            agent_loop(\"hello\", nil, {provider: \"mock\"})";
                let stop_reason = run_acp_agent_loop_prompt(body).await;
                assert_eq!(stop_reason, "refusal");
            })
            .await;
}

#[tokio::test(flavor = "current_thread")]
async fn acp_session_prompt_reports_max_turn_requests_when_iteration_cap_hit() {
    let local = tokio::task::LocalSet::new();
    local
            .run_until(async {
                // `loop_until_done: true` keeps the loop iterating on a
                // text-only mock turn, and `max_iterations: 1` forces
                // the cap to fire on iteration 1 → ACP `max_turn_requests`.
                let body = "llm_mock_clear()\n\
                            llm_mock({text: \"still working\"})\n\
                            llm_mock({text: \"still working\"})\n\
                            agent_loop(\"hello\", nil, {provider: \"mock\", loop_until_done: true, max_iterations: 1})";
                let stop_reason = run_acp_agent_loop_prompt(body).await;
                assert_eq!(stop_reason, "max_turn_requests");
            })
            .await;
}
