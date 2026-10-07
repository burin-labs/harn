//! A session's spend ceiling covers the whole session.
//!
//! Every prompt installs a fresh cost scope, and a resumed session runs in a
//! new process. Before the session carried its spend between them, each turn
//! started at $0: a session that had already spent past its cap was admitted
//! again on the next prompt and again after every `--continue`.

use super::event_log_barrier::ResetActiveEventLog;
use super::*;

const CALL_COST_USD: f64 = 0.02;
const CAP_USD: f64 = 0.03;

struct MockModeGuard;

impl Drop for MockModeGuard {
    fn drop(&mut self) {
        harn_vm::llm::clear_cli_llm_mock_mode();
    }
}

/// Each model call costs exactly `CALL_COST_USD`, so the arithmetic is the
/// test's, not the pricing catalog's.
fn install_priced_mocks(count: usize) -> MockModeGuard {
    let mocks = (0..count)
        .map(|_| {
            harn_vm::llm::parse_llm_mock_value(&serde_json::json!({
                "text": "ok",
                "model": "gpt-4o-mini",
                "provider": "mock",
                "simulated_cost_usd": CALL_COST_USD,
            }))
            .expect("mock fixture")
        })
        .collect();
    harn_vm::llm::install_cli_llm_mocks(mocks);
    MockModeGuard
}

fn project_with_pipeline() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let project = dir.path().join("project");
    std::fs::create_dir_all(project.join(".harn")).expect("project state dir");
    let pipeline = project.join("spend.harn");
    std::fs::write(
        &pipeline,
        r#"
pipeline main(harness: Harness) {
  const outcome: string = try {
    harness.llm.call("Reply briefly", nil, {provider: "mock", model: "gpt-4o-mini", max_tokens: 8})
    "admitted"
  } catch (error) {
    "refused"
  }
  harness.stdio.println(outcome + " " + to_string(harness.llm.session_cost()?.budget_charged_usd))
}
"#,
    )
    .expect("write pipeline");
    (dir, project, pipeline)
}

fn capped_config(pipeline: &Path) -> AcpServerConfig {
    AcpServerConfig::new(Some(pipeline.to_string_lossy().into_owned())).with_budget(BudgetSpec {
        llm_cost_usd: Some(CAP_USD),
        ..BudgetSpec::default()
    })
}

/// Run one prompt and return what the pipeline printed.
async fn run_prompt(
    request_tx: &mpsc::UnboundedSender<serde_json::Value>,
    response_rx: &mut mpsc::UnboundedReceiver<String>,
    session_id: &str,
    request_id: u64,
) -> String {
    request_tx
        .send(serde_json::json!({
            "jsonrpc": "2.0", "id": request_id, "method": "session/prompt",
            "params": {"sessionId": session_id, "prompt": [{"type": "text", "text": "go"}]},
        }))
        .expect("send session/prompt");
    let mut output = String::new();
    for _ in 0..128 {
        let message = recv_json(response_rx).await;
        if message["method"] == "host/capabilities" {
            request_tx
                .send(serde_json::json!({"jsonrpc": "2.0", "id": message["id"], "result": {}}))
                .expect("send host capabilities response");
        } else if message["params"]["update"]["sessionUpdate"] == "agent_message_chunk" {
            output.push_str(
                message["params"]["update"]["content"]["text"]
                    .as_str()
                    .unwrap_or_default(),
            );
        } else if message["id"] == request_id {
            assert!(message.get("error").is_none(), "prompt failed: {message}");
            return output.trim().to_string();
        }
    }
    panic!("prompt {request_id} never answered; output so far: {output}");
}

async fn stored_spend_micros(project: &Path, session_id: &str) -> u64 {
    use harn_session_store::SessionStore;
    harn_vm::open_canonical_store(project)
        .expect("canonical store")
        .describe(session_id)
        .await
        .expect("the session row")
        .usage_cost_usd_micros
}

#[tokio::test(flavor = "current_thread")]
async fn a_later_prompt_is_charged_what_earlier_prompts_spent() {
    let _reset = ResetActiveEventLog;
    harn_vm::reset_thread_local_state();
    let _mocks = install_priced_mocks(3);
    let (_dir, project, pipeline) = project_with_pipeline();
    tokio::task::LocalSet::new()
        .run_until(async {
            let (request_tx, mut response_rx, server, session_id) =
                start_acp_channel_session_with_config(
                    capped_config(&pipeline),
                    serde_json::json!(project),
                )
                .await;
            // An interactive host records the session row when it opens the
            // session; the prompt path only updates a row that exists.
            {
                use harn_session_store::{CreateSession, SessionStore};
                harn_vm::open_canonical_store(&project)
                    .expect("canonical store")
                    .create(CreateSession {
                        id: Some(session_id.clone()),
                        ..CreateSession::default()
                    })
                    .await
                    .expect("create the session row");
            }

            // 2c of a 3c cap.
            let first = run_prompt(&request_tx, &mut response_rx, &session_id, 2).await;
            assert!(first.starts_with("admitted"), "{first}");
            assert_eq!(stored_spend_micros(&project, &session_id).await, 20_000);

            // The session has 1c left; the next 2c call takes it over the cap.
            let second = run_prompt(&request_tx, &mut response_rx, &session_id, 3).await;
            assert!(
                second.starts_with("refused"),
                "the second prompt must be charged the first prompt's spend: {second}"
            );
            assert_eq!(stored_spend_micros(&project, &session_id).await, 40_000);

            // Raising the cap between turns (a host's "raise and
            // continue") has to reach the next turn, not only the live scope
            // that the turn's end already discarded.
            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0", "method": "session/set_budget",
                    "params": {"sessionId": session_id, "llm_cost_usd": 0.1, "llm_tokens": null},
                }))
                .expect("send session/set_budget");
            let third = run_prompt(&request_tx, &mut response_rx, &session_id, 4).await;
            assert!(third.starts_with("admitted"), "{third}");
            assert_eq!(stored_spend_micros(&project, &session_id).await, 60_000);

            drop(request_tx);
            server.await.expect("ACP channel server task");
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_resumed_session_is_charged_what_it_spent_before() {
    let _reset = ResetActiveEventLog;
    harn_vm::reset_thread_local_state();
    let _mocks = install_priced_mocks(1);
    let (_dir, project, pipeline) = project_with_pipeline();
    let session_id = "01a003d0-1513-7271-90aa-4542d6059499";
    {
        use harn_session_store::{CreateSession, SessionStore};
        // What a previous process left: 2.5c spent against the 3c cap.
        harn_vm::open_canonical_store(&project)
            .expect("canonical store")
            .create(CreateSession {
                id: Some(session_id.to_string()),
                usage_cost_usd_micros: 25_000,
                ..CreateSession::default()
            })
            .await
            .expect("create stored session");
    }
    tokio::task::LocalSet::new()
        .run_until(async {
            let (request_tx, request_rx) = mpsc::unbounded_channel();
            let (response_tx, mut response_rx) = mpsc::unbounded_channel();
            let server = tokio::task::spawn_local(super::run_acp_channel_server(
                capped_config(&pipeline),
                request_rx,
                response_tx,
            ));
            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0", "id": 1, "method": "session/load",
                    "params": {"sessionId": session_id, "cwd": project.display().to_string(),
                        "environmentPolicy": {"kind": "isolated"}},
                }))
                .expect("send session/load");
            loop {
                let message = recv_json(&mut response_rx).await;
                if message["id"] == 1 {
                    assert!(message.get("error").is_none(), "load failed: {message}");
                    break;
                }
            }

            let resumed = run_prompt(&request_tx, &mut response_rx, session_id, 2).await;
            assert!(
                resumed.starts_with("refused"),
                "a resumed session must be charged its earlier spend: {resumed}"
            );
            assert_eq!(stored_spend_micros(&project, session_id).await, 45_000);

            drop(request_tx);
            server.await.expect("ACP channel server task");
        })
        .await;
}

/// A session recorded before its row carried the total: the calls are on
/// record, the row says $0. Loading it must charge the recorded calls, and
/// write them to the row the client reads for its header.
#[tokio::test(flavor = "current_thread")]
async fn loading_an_older_session_backfills_its_spend_from_recorded_calls() {
    let _reset = ResetActiveEventLog;
    harn_vm::reset_thread_local_state();
    let (_dir, project, pipeline) = project_with_pipeline();
    let session_id = "01a003d0-1513-7271-90aa-4542d605949a";
    {
        use harn_session_store::{AppendEvent, CreateSession, SessionEventKind, SessionStore};
        let store = harn_vm::open_canonical_store(&project).expect("canonical store");
        store
            .create(CreateSession {
                id: Some(session_id.to_string()),
                ..CreateSession::default()
            })
            .await
            .expect("create stored session");
        for cost_usd in [0.01, 0.015] {
            store
                .append(
                    session_id,
                    AppendEvent::new(
                        SessionEventKind::Custom {
                            custom_type: "llm_call".to_string(),
                        },
                        serde_json::json!({
                            "transcript_event": {
                                "kind": "llm_call",
                                "metadata": {"model": "gpt-4o-mini", "cost_usd": cost_usd},
                            }
                        }),
                    ),
                )
                .await
                .expect("append recorded call");
        }
    }
    assert_eq!(stored_spend_micros(&project, session_id).await, 0);
    tokio::task::LocalSet::new()
        .run_until(async {
            let (request_tx, request_rx) = mpsc::unbounded_channel();
            let (response_tx, mut response_rx) = mpsc::unbounded_channel();
            let server = tokio::task::spawn_local(super::run_acp_channel_server(
                capped_config(&pipeline),
                request_rx,
                response_tx,
            ));
            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0", "id": 1, "method": "session/load",
                    "params": {"sessionId": session_id, "cwd": project.display().to_string(),
                        "environmentPolicy": {"kind": "isolated"}},
                }))
                .expect("send session/load");
            loop {
                let message = recv_json(&mut response_rx).await;
                if message["id"] == 1 {
                    assert!(message.get("error").is_none(), "load failed: {message}");
                    break;
                }
            }
            assert_eq!(stored_spend_micros(&project, session_id).await, 25_000);
            drop(request_tx);
            server.await.expect("ACP channel server task");
        })
        .await;
}

/// When the session's earlier spend cannot be read, only a capped turn is
/// refused: it cannot be held to a ceiling it cannot measure. An uncapped turn
/// has nothing to enforce and still runs.
#[tokio::test(flavor = "current_thread")]
async fn an_unreadable_spend_refuses_only_a_capped_turn() {
    let _reset = ResetActiveEventLog;
    harn_vm::reset_thread_local_state();
    let _mocks = install_priced_mocks(1);
    let (_dir, project, pipeline) = project_with_pipeline();
    // A directory where the store file belongs: every open of it fails.
    std::fs::create_dir_all(project.join(".harn/session-store.sqlite")).expect("block the store");
    for (config, capped) in [
        (
            AcpServerConfig::new(Some(pipeline.to_string_lossy().into_owned())),
            false,
        ),
        (capped_config(&pipeline), true),
    ] {
        tokio::task::LocalSet::new()
            .run_until(async {
                let (request_tx, mut response_rx, server, session_id) =
                    start_acp_channel_session_with_config(config, serde_json::json!(project))
                        .await;
                request_tx
                    .send(serde_json::json!({
                        "jsonrpc": "2.0", "id": 2, "method": "session/prompt",
                        "params": {"sessionId": session_id, "prompt": [{"type": "text", "text": "go"}]},
                    }))
                    .expect("send session/prompt");
                let response = loop {
                    let message = recv_json(&mut response_rx).await;
                    if message["method"] == "host/capabilities" {
                        request_tx
                            .send(serde_json::json!({"jsonrpc": "2.0", "id": message["id"], "result": {}}))
                            .expect("send host capabilities response");
                    } else if message["id"] == 2 {
                        break message;
                    }
                };
                assert_eq!(
                    response.get("error").is_some(),
                    capped,
                    "capped={capped}: {response}"
                );
                drop(request_tx);
                server.await.expect("ACP channel server task");
            })
            .await;
    }
}
