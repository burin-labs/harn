//! `session/remind` wakes a daemon that is idle inside its prompt.
//!
//! A daemon agent loop never returns from `session/prompt`: a turn without a
//! tool call idles in `daemon_wait` until an `interrupt_immediate` injection
//! lands. The dispatcher is busy with that prompt for the session's whole life,
//! so a reminder only the dispatcher reads would wait for a prompt that never
//! ends. The router must queue it on the live session instead.

use super::*;

/// Generous bound on each awaited event; reached only when the event never comes.
const EVENT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

const DAEMON_PIPELINE: &str = r#"import { agent_loop } from "std/agent/loop"

pipeline default(harness: Harness, task: unknown) {
  harness.llm.mock_clear()
  harness.llm.mock_enqueue({text: "IDLE-ONE"})
  harness.llm.mock_enqueue({text: "WOKEN-TWO"})
  agent_loop(harness, prompt, nil, {
    provider: "mock",
    daemon: true,
    wake_interval_ms: 20,
    max_iterations: 4,
  })
}
"#;

/// Read frames until one carries the model text `needle`, answering any
/// host capability probe on the way. Returns the frames read.
async fn read_until_chunk(
    tx: &mpsc::UnboundedSender<serde_json::Value>,
    rx: &mut mpsc::UnboundedReceiver<String>,
    needle: &str,
) -> Vec<serde_json::Value> {
    let mut seen = Vec::new();
    let found = tokio::time::timeout(EVENT_TIMEOUT, async {
        loop {
            let Some(line) = rx.recv().await else {
                return false;
            };
            let Ok(message) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            if message["method"] == "host/capabilities" {
                tx.send(serde_json::json!({"jsonrpc":"2.0", "id":message["id"], "result":{}}))
                    .unwrap();
            }
            // The mock's reply text is unique to the model call that returns it.
            let chunk = message.to_string().contains(needle);
            seen.push(message);
            if chunk {
                return true;
            }
        }
    })
    .await
    .unwrap_or(false);
    assert!(found, "no `{needle}` model text arrived; frames: {seen:#?}");
    seen
}

#[tokio::test(flavor = "current_thread")]
async fn a_reminder_wakes_a_daemon_idle_inside_its_prompt() {
    harn_clock::test_support::with_local_tasks(async {
        let dir = tempfile::tempdir().expect("tempdir");
        let pipeline = dir.path().join("daemon.harn");
        std::fs::write(&pipeline, DAEMON_PIPELINE).expect("write daemon pipeline");
        let (tx, mut rx, server, session_id) = start_acp_code_session_with_config(
            AcpServerConfig::for_pipeline(pipeline.to_string_lossy().to_string()),
            serde_json::json!(dir.path()),
        )
        .await;
        tx.send(serde_json::json!({
            "jsonrpc":"2.0", "id":20, "method":"session/prompt",
            "params":{"sessionId":session_id, "prompt":[{"type":"text", "text":"Watch for work."}]},
        }))
        .unwrap();
        read_until_chunk(&tx, &mut rx, "IDLE-ONE").await;

        // The turn ended without a tool call, so the daemon is idle and the
        // prompt is still in flight. Only the reminder can start the next turn.
        tx.send(serde_json::json!({
            "jsonrpc":"2.0", "method":"session/remind",
            "params":{
                "sessionId":session_id,
                "mode":"interrupt_immediate",
                "reminder":{"body":"A worker finished.", "tags":["wake"]},
            },
        }))
        .unwrap();
        let frames = read_until_chunk(&tx, &mut rx, "WOKEN-TWO").await;
        assert!(
            !frames.iter().any(|frame| frame["id"] == 20),
            "the prompt ended instead of idling: {frames:#?}"
        );

        tx.send(serde_json::json!({
            "jsonrpc":"2.0", "method":"session/cancel", "params":{"sessionId":session_id},
        }))
        .unwrap();
        // A daemon turn streams many events, so read to the prompt's own reply.
        let terminal = tokio::time::timeout(EVENT_TIMEOUT, async {
            loop {
                let message = recv_json(&mut rx).await;
                if message["id"] == 20 {
                    return message;
                }
            }
        })
        .await
        .expect("the cancelled prompt never answered");
        assert_eq!(terminal["result"]["stopReason"], "cancelled", "{terminal}");
        drop(tx);
        server.await.unwrap();
    })
    .await;
}

/// Wait for an `agent_message_chunk` whose text is `needle`, failing if the
/// prompt answers first. Model text also rides on iteration events, so only a
/// published chunk counts as a reply the client can show.
async fn read_until_reply_chunk(
    tx: &mpsc::UnboundedSender<serde_json::Value>,
    rx: &mut mpsc::UnboundedReceiver<String>,
    needle: &str,
    prompt_id: u64,
) {
    let mut seen = Vec::new();
    let found = tokio::time::timeout(EVENT_TIMEOUT, async {
        loop {
            let Some(line) = rx.recv().await else {
                return false;
            };
            let Ok(message) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            if message["method"] == "host/capabilities" {
                tx.send(serde_json::json!({"jsonrpc":"2.0", "id":message["id"], "result":{}}))
                    .unwrap();
            }
            assert_ne!(
                message["id"], prompt_id,
                "the prompt ended before its reply was published: {seen:#?}"
            );
            let update = &message["params"]["update"];
            let reply = update["sessionUpdate"] == "agent_message_chunk"
                && update["content"]["text"]
                    .as_str()
                    .is_some_and(|text| text.contains(needle));
            seen.push(message);
            if reply {
                return true;
            }
        }
    })
    .await
    .unwrap_or(false);
    assert!(
        found,
        "no `{needle}` reply chunk while the daemon idled; frames: {seen:#?}"
    );
}

/// A daemon idles after a toolless reply instead of finalizing, and a reply
/// was published only at finalization, so a client never saw a daemon's
/// replies until the session ended. The reply it idles on is published before
/// it waits.
#[tokio::test(flavor = "current_thread")]
async fn a_daemon_publishes_the_reply_it_idles_on() {
    harn_clock::test_support::with_local_tasks(async {
        let dir = tempfile::tempdir().expect("tempdir");
        let pipeline = dir.path().join("daemon.harn");
        std::fs::write(&pipeline, DAEMON_PIPELINE).expect("write daemon pipeline");
        let (tx, mut rx, server, session_id) = start_acp_code_session_with_config(
            AcpServerConfig::for_pipeline(pipeline.to_string_lossy().to_string()),
            serde_json::json!(dir.path()),
        )
        .await;
        tx.send(serde_json::json!({
            "jsonrpc":"2.0", "id":21, "method":"session/prompt",
            "params":{"sessionId":session_id, "prompt":[{"type":"text", "text":"Watch for work."}]},
        }))
        .unwrap();
        read_until_reply_chunk(&tx, &mut rx, "IDLE-ONE", 21).await;

        tx.send(serde_json::json!({
            "jsonrpc":"2.0", "method":"session/cancel", "params":{"sessionId":session_id},
        }))
        .unwrap();
        let terminal = tokio::time::timeout(EVENT_TIMEOUT, async {
            loop {
                let message = recv_json(&mut rx).await;
                if message["id"] == 21 {
                    return message;
                }
            }
        })
        .await
        .expect("the cancelled prompt never answered");
        assert_eq!(terminal["result"]["stopReason"], "cancelled", "{terminal}");
        drop(tx);
        server.await.unwrap();
    })
    .await;
}

#[test]
fn an_active_reminder_lands_on_the_live_session_and_requires_authentication() {
    let controls = ConcurrentSessionControls::new(false, serde_json::json!({}));
    let control = ConcurrentSessionControl::new();
    control.set_prompt_active(true);
    controls.register("active", control.clone());
    let captured = Arc::new(Mutex::new(Vec::<String>::new()));
    let lines = captured.clone();
    let output = AcpOutput::callback(move |line| lines.lock().unwrap().push(line.into()));
    let message = serde_json::json!({"jsonrpc":"2.0", "id":1, "method":"session/remind", "params":{
        "sessionId":"active", "mode":"interrupt_immediate", "reminder":{"body":"wake"},
    }});
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    runtime.block_on(async {
        assert!(
            controls
                .preempt_active_session_remind(&message, &output)
                .await
        );
        assert_eq!(
            control.inject_state.pending_injections_json().await["pendingCount"],
            0,
            "an unauthenticated reminder must not be queued"
        );
        controls.mark_authenticated();
        assert!(
            controls
                .preempt_active_session_remind(&message, &output)
                .await
        );
        let pending = control.inject_state.pending_injections_json().await;
        assert_eq!(pending["pendingCount"], 1, "{pending}");
        // Between prompts the dispatcher still owns the request.
        control.set_prompt_active(false);
        assert!(
            !controls
                .preempt_active_session_remind(&message, &output)
                .await
        );
    });
    let replies = captured.lock().unwrap().clone();
    assert_eq!(replies.len(), 2, "{replies:?}");
    assert!(replies[1].contains("reminderId"), "{replies:?}");
}
