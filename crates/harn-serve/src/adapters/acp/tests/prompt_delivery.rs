//! `session/prompt` with `delivery` over a real ACP channel session.
//!
//! Each test runs a mocked agent loop whose `tick` tool blocks until a gate
//! file exists, so "before insertion" is a state the test holds open rather
//! than a race it hopes to win. A prompt whose text is `hold` waits on the same
//! gate without running any loop, so no checkpoint can insert a steer.

use super::*;

const TICKS: usize = 3;

struct DeliveryHarness {
    _dir: tempfile::TempDir,
    checkpoint_tracking: bool,
    gate: std::path::PathBuf,
    ticks: std::path::PathBuf,
    tx: mpsc::UnboundedSender<serde_json::Value>,
    rx: mpsc::UnboundedReceiver<String>,
    server: tokio::task::JoinHandle<()>,
    session_id: String,
    seen: Vec<serde_json::Value>,
}

fn harn_path(path: &std::path::Path) -> String {
    format!(
        "\"{}\"",
        harn_lexer::escape_string_literal(&path.to_string_lossy())
    )
}

impl DeliveryHarness {
    async fn start() -> Self {
        Self::start_with_checkpoint(false).await
    }

    async fn start_with_checkpoint(checkpoint: bool) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let gate = dir.path().join("gate");
        let ticks = dir.path().join("ticks.txt");
        let pipeline = dir.path().join("delivery.harn");
        let mut mocks = String::new();
        for index in 0..TICKS {
            mocks.push_str(&format!(
                "  harness.llm.mock_enqueue({{text: \"\", tool_calls: \
                 [{{id: \"c{index}\", name: \"tick\", arguments: {{n: {index}}}}}]}})\n"
            ));
        }
        let (gate_literal, ticks_literal) = (harn_path(&gate), harn_path(&ticks));
        let source = format!(
            r#"import {{ agent_loop }} from "std/agent/loop"

fn wait_for_gate(harness: Harness) {{
  while !harness.fs.exists({gate_literal}) {{
    harness.clock.sleep_ms(10)
  }}
}}

fn tick_tools(harness: Harness) {{
  let tools = tool_registry()
  tools = tool_define(
    tools,
    "tick",
    "Record one tick once the gate opens",
    {{
      handler: {{ args ->
        harness.fs.append({ticks_literal}, "tick\n")
        wait_for_gate(harness)
        "ticked"
      }},
      parameters: {{n: {{type: "number", description: "Tick index"}}}},
      returns: {{type: "string"}},
      annotations: {{kind: "read"}},
    }},
  )
  return tools
}}

pipeline default(harness: Harness, task: unknown) {{
  if prompt == "hold" {{
    wait_for_gate(harness)
    return
  }}
  harness.llm.mock_clear()
{mocks}  harness.llm.mock_enqueue({{text: "done"}})
  agent_loop(harness, prompt, nil, {{
    provider: "mock",
    tools: tick_tools(harness),
    tool_format: "native",
    max_iterations: {max_iterations},
  }})
}}
"#,
            max_iterations = TICKS + 4,
        );
        std::fs::write(&pipeline, source).expect("write delivery pipeline");
        let config = AcpServerConfig::for_pipeline(pipeline.to_string_lossy().to_string());
        let (tx, rx, server, session_id) = if checkpoint {
            for args in [
                vec!["init", "-q"],
                vec!["config", "user.email", "t@example.com"],
                vec!["config", "user.name", "t"],
                vec!["add", "-A"],
                vec!["commit", "-q", "-m", "init"],
            ] {
                assert!(std::process::Command::new("git")
                    .arg("-C")
                    .arg(dir.path())
                    .args(args)
                    .status()
                    .unwrap()
                    .success());
            }
            let (tx, mut rx, server, session_id) = start_acp_channel_session_with_params(
                config,
                serde_json::json!({"cwd":dir.path(), "environmentPolicy":{"kind":"isolated", "grants":[]}, "workspaceCheckpoint":true}),
            ).await;
            tx.send(serde_json::json!({
                "jsonrpc":"2.0", "id":2, "method":"session/set_mode",
                "params":{"sessionId":session_id, "modeId":"code"},
            }))
            .unwrap();
            let _ack = recv_response_with_id(&mut rx, 2).await;
            (tx, rx, server, session_id)
        } else {
            start_acp_code_session_with_config(config, serde_json::json!(dir.path())).await
        };
        Self {
            _dir: dir,
            checkpoint_tracking: checkpoint,
            gate,
            ticks,
            tx,
            rx,
            server,
            session_id,
            seen: Vec::new(),
        }
    }

    fn open_gate(&self) {
        std::fs::write(&self.gate, "").expect("open gate");
    }

    fn send(&self, message: serde_json::Value) {
        self.tx.send(message).expect("send ACP message");
    }

    fn prompt(&self, id: u64, text: &str, delivery: Option<&str>) {
        let mut params = serde_json::json!({
            "sessionId": self.session_id,
            "prompt": [{"type": "text", "text": text}],
        });
        if let Some(delivery) = delivery {
            params["delivery"] = serde_json::json!(delivery);
        }
        if delivery.is_some() || self.checkpoint_tracking {
            params["messageId"] = serde_json::json!(format!("m{id}"));
        }
        self.send(serde_json::json!({
            "jsonrpc": "2.0", "id": id, "method": "session/prompt", "params": params,
        }));
    }

    fn cancel_request(&self, request_id: u64) {
        self.send(serde_json::json!({
            "jsonrpc": "2.0", "method": "$/cancel_request", "params": {"requestId": request_id},
        }));
    }

    async fn read_one(&mut self, what: &str) {
        let line = harn_clock::test_support::within(what, self.rx.recv())
            .await
            .unwrap_or_else(|| panic!("ACP channel closed waiting for {what}"));
        let message: serde_json::Value = serde_json::from_str(&line).expect("ACP JSON");
        if message["method"] == "host/capabilities" {
            reply_empty_result(&self.tx, &message);
        }
        self.seen.push(message);
    }

    /// Read until the `nth` (zero-based) match, answering host callbacks on
    /// the way. Returns its index in `seen`.
    async fn until_nth(
        &mut self,
        what: &str,
        nth: usize,
        matches: impl Fn(&serde_json::Value) -> bool,
    ) -> usize {
        loop {
            let found = self
                .seen
                .iter()
                .enumerate()
                .filter(|(_, message)| matches(message))
                .nth(nth);
            if let Some((index, _)) = found {
                return index;
            }
            self.read_one(what).await;
        }
    }

    async fn reply(&mut self, id: u64) -> (usize, serde_json::Value) {
        let index = self
            .until_nth(&format!("reply {id}"), 0, |message| {
                message["id"] == id && message.get("method").is_none()
            })
            .await;
        (index, self.seen[index].clone())
    }

    async fn ticks_reach(&mut self, count: usize) {
        // Tick recording is a file write inside the VM, not an ACP message, so
        // yield to the runtime (draining host callbacks) until the file shows
        // the count; the hang ceiling fails a loop that never ticks.
        let what = format!("the loop to reach {count} ticks");
        harn_clock::test_support::within(&what, async {
            loop {
                let ticks = std::fs::read_to_string(&self.ticks)
                    .map(|text| text.lines().count())
                    .unwrap_or(0);
                if ticks >= count {
                    return;
                }
                while let Ok(line) = self.rx.try_recv() {
                    let message: serde_json::Value = serde_json::from_str(&line).expect("ACP JSON");
                    if message["method"] == "host/capabilities" {
                        reply_empty_result(&self.tx, &message);
                    }
                    self.seen.push(message);
                }
                tokio::task::yield_now().await;
            }
        })
        .await;
    }

    async fn state_update(&mut self, nth: usize, state: &str) -> (usize, serde_json::Value) {
        let index = self
            .until_nth(&format!("state_update {state} #{nth}"), nth, |message| {
                is_state_update(message, state)
            })
            .await;
        (index, self.seen[index]["params"]["update"].clone())
    }

    fn user_message(&self, message_id: &str) -> Option<usize> {
        self.seen.iter().position(|message| {
            message["params"]["update"]["sessionUpdate"] == "user_message"
                && message["params"]["update"]["messageId"] == message_id
        })
    }

    async fn finish(self) -> Vec<serde_json::Value> {
        drop(self.tx);
        let _ = self.server.await;
        self.seen
    }
}

fn is_state_update(message: &serde_json::Value, state: &str) -> bool {
    message["params"]["update"]["sessionUpdate"] == "state_update"
        && message["params"]["update"]["state"] == state
}

fn assert_cancelled(reply: &serde_json::Value) {
    assert_eq!(
        reply["error"]["code"], ACP_REQUEST_CANCELLED_CODE,
        "{reply}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn steer_is_inserted_mid_turn_and_answered_at_insertion() {
    harn_clock::test_support::with_local_tasks(async {
        let mut h = DeliveryHarness::start().await;
        h.prompt(3, "tick", None);
        h.ticks_reach(1).await;
        h.prompt(4, "change course", Some("steer"));

        // The tool is still blocked, so the steer is pending, not inserted.
        h.send(serde_json::json!({
            "jsonrpc": "2.0", "id": 5, "method": "session/pending_prompts",
            "params": {"sessionId": h.session_id},
        }));
        let (_, pending) = h.reply(5).await;
        assert_eq!(pending["result"]["prompts"][0]["messageId"], "m4");
        assert_eq!(pending["result"]["prompts"][0]["delivery"], "steer");
        assert!(h.seen.iter().all(|message| message["id"] != 4));

        h.open_gate();
        let (receipt_at, receipt) = h.reply(4).await;
        assert_eq!(receipt["result"]["messageId"], "m4");
        let inserted_at = h.user_message("m4").expect("steer reached the transcript");
        assert!(inserted_at < receipt_at, "receipt preceded insertion");

        let (turn_end_at, turn_end) = h.reply(3).await;
        assert_eq!(turn_end["result"]["stopReason"], "end_turn");
        assert!(
            receipt_at < turn_end_at,
            "the steer was answered at turn end"
        );
        let seen = h.finish().await;
        assert!(
            seen.iter()
                .all(|message| !is_state_update(message, "running")),
            "a steer that joined a turn must not start one"
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn queued_prompt_starts_its_own_turn_after_the_running_one() {
    harn_clock::test_support::with_local_tasks(async {
        let mut h = DeliveryHarness::start().await;
        h.prompt(3, "tick", None);
        h.ticks_reach(1).await;
        h.prompt(4, "after this", Some("queue"));
        h.open_gate();

        let (first_end_at, first_end) = h.reply(3).await;
        assert_eq!(first_end["result"]["stopReason"], "end_turn");
        assert!(
            h.user_message("m4").is_none(),
            "a queued prompt joined the running turn"
        );
        let (receipt_at, receipt) = h.reply(4).await;
        assert_eq!(receipt["result"]["messageId"], "m4");
        assert!(first_end_at < receipt_at);
        let (running_at, _) = h.state_update(0, "running").await;
        let (idle_at, idle) = h.state_update(0, "idle").await;
        assert_eq!(idle["stopReason"], "end_turn");
        let inserted_at = h.user_message("m4").expect("queued prompt opened its turn");
        assert!(
            running_at < inserted_at && inserted_at < receipt_at,
            "receipt preceded the queued prompt's insertion"
        );
        assert!(receipt_at < idle_at);
        h.finish().await;
    })
    .await;
}

#[cfg(feature = "hostlib")]
#[tokio::test(flavor = "current_thread")]
async fn queued_prompt_captures_its_boundary_when_the_turn_starts() {
    harn_clock::test_support::with_local_tasks(async {
        use harn_hostlib::workspace_checkpoint as checkpoint;
        let mut h = DeliveryHarness::start_with_checkpoint(true).await;
        h.prompt(3, "tick", None);
        h.ticks_reach(1).await;
        h.prompt(4, "after this", Some("queue"));
        h.send(serde_json::json!({
            "jsonrpc":"2.0", "id":5, "method":"session/pending_prompts",
            "params":{"sessionId":h.session_id},
        }));
        let (_, pending) = h.reply(5).await;
        assert_eq!(pending["result"]["prompts"][0]["messageId"], "m4");
        h.open_gate();
        let (_, first) = h.reply(3).await;
        assert_eq!(first["result"]["stopReason"], "end_turn");
        let (_, receipt) = h.reply(4).await;
        assert_eq!(receipt["result"]["messageId"], "m4");
        h.state_update(0, "idle").await;

        let begin = checkpoint::call_begin(&h.session_id, "probe").unwrap();
        assert!(matches!(begin, checkpoint::Outcome::Ready(_)), "{begin:?}");
        std::fs::write(&h.ticks, "later probe\n").unwrap();
        let end = checkpoint::call_end(&h.session_id, "probe", None).unwrap();
        assert!(matches!(end, checkpoint::Outcome::Ready(_)), "{end:?}");
        let restored = checkpoint::restore(&h.session_id, "m4", None).unwrap();
        assert!(
            matches!(restored, checkpoint::Outcome::Ready(_)),
            "{restored:?}"
        );
        assert_eq!(
            std::fs::read_to_string(&h.ticks).unwrap(),
            "tick\n".repeat(TICKS)
        );
        checkpoint::drop_session(&h.session_id);
        h.finish().await;
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn delivery_prompt_to_an_idle_session_starts_a_turn() {
    harn_clock::test_support::with_local_tasks(async {
        let mut h = DeliveryHarness::start().await;
        h.open_gate();
        h.prompt(3, "tick", Some("steer"));

        let (receipt_at, receipt) = h.reply(3).await;
        assert_eq!(receipt["result"]["messageId"], "m3");
        let (running_at, _) = h.state_update(0, "running").await;
        let (idle_at, idle) = h.state_update(0, "idle").await;
        let inserted_at = h.user_message("m3").expect("idle prompt opened its turn");
        assert!(
            running_at < inserted_at && inserted_at < receipt_at,
            "receipt preceded the idle prompt's insertion"
        );
        assert!(receipt_at < idle_at);
        assert_eq!(idle["stopReason"], "end_turn");
        assert!(
            idle["_meta"]["harn"]["terminal"]["kind"].is_string(),
            "{idle}"
        );
        let seen = h.finish().await;
        assert_eq!(
            seen.iter().filter(|message| message["id"] == 3).count(),
            1,
            "a delivery prompt gets one response: its receipt"
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn unsupported_delivery_and_removed_inject_methods_are_refused() {
    harn_clock::test_support::with_local_tasks(async {
        let mut h = DeliveryHarness::start().await;
        for (id, delivery) in [
            (3, serde_json::json!("interrupt")),
            (4, serde_json::json!(7)),
        ] {
            h.send(serde_json::json!({
                "jsonrpc": "2.0", "id": id, "method": "session/prompt",
                "params": {
                    "sessionId": h.session_id,
                    "delivery": delivery,
                    "prompt": [{"type": "text", "text": "tick"}],
                },
            }));
            let (_, reply) = h.reply(id).await;
            assert_eq!(reply["error"]["code"], -32602, "{reply}");
        }
        for (id, method) in [
            (5, "session/inject"),
            (6, "session/revoke_inject"),
            (7, "session/replace_inject"),
            (8, "session/pending_injections"),
        ] {
            h.send(serde_json::json!({
                "jsonrpc": "2.0", "id": id, "method": method,
                "params": {"sessionId": h.session_id, "content": "x", "messageId": "m"},
            }));
            let (_, reply) = h.reply(id).await;
            assert_eq!(reply["error"]["code"], -32601, "{method}: {reply}");
        }
        let seen = h.finish().await;
        assert!(seen
            .iter()
            .all(|message| !is_state_update(message, "running")));
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn cancel_request_withdraws_only_a_prompt_not_yet_inserted() {
    harn_clock::test_support::with_local_tasks(async {
        let mut h = DeliveryHarness::start().await;
        h.prompt(3, "tick", None);
        h.ticks_reach(1).await;
        h.prompt(4, "never mind", Some("steer"));
        h.cancel_request(4);
        let (_, withdrawn) = h.reply(4).await;
        assert_cancelled(&withdrawn);

        h.prompt(5, "keep going", Some("steer"));
        h.open_gate();
        let (_, receipt) = h.reply(5).await;
        assert_eq!(receipt["result"]["messageId"], "m5");
        h.cancel_request(5);
        let (_, turn_end) = h.reply(3).await;
        assert_eq!(turn_end["result"]["stopReason"], "end_turn");

        let seen = h.finish().await;
        let replies = |id: u64| seen.iter().filter(|message| message["id"] == id).count();
        assert_eq!((replies(4), replies(5)), (1, 1));
        assert!(
            seen.iter()
                .all(|message| message["params"]["update"]["messageId"] != "m4"),
            "a withdrawn steer reached the transcript"
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn session_cancel_keeps_pending_prompts_for_the_next_turns() {
    harn_clock::test_support::with_local_tasks(async {
        let mut h = DeliveryHarness::start().await;
        h.prompt(3, "tick", None);
        h.ticks_reach(1).await;
        h.prompt(4, "steer me", Some("steer"));
        h.prompt(5, "queue me", Some("queue"));
        h.send(serde_json::json!({
            "jsonrpc": "2.0", "method": "session/cancel",
            "params": {"sessionId": h.session_id},
        }));
        h.open_gate();
        let (cancelled_at, turn_end) = h.reply(3).await;
        assert_eq!(turn_end["result"]["stopReason"], "cancelled");

        let (steer_at, steer) = h.reply(4).await;
        assert_eq!(steer["result"]["messageId"], "m4");
        let (steer_turn_at, _) = h.state_update(0, "running").await;
        let (steer_end_at, _) = h.state_update(0, "idle").await;
        let (queued_at, queued) = h.reply(5).await;
        assert_eq!(queued["result"]["messageId"], "m5");
        let (queue_turn_at, _) = h.state_update(1, "running").await;
        let m4 = h.user_message("m4").expect("steer inserted");
        let m5 = h.user_message("m5").expect("queued prompt inserted");
        assert!(
            cancelled_at < steer_turn_at && steer_turn_at < m4 && m4 < steer_at,
            "the pending steer opens the turn after the cancelled one, receipt after insertion"
        );
        assert!(steer_at < steer_end_at);
        assert!(
            steer_end_at < queue_turn_at && queue_turn_at < m5 && m5 < queued_at,
            "the queued prompt's receipt follows its insertion"
        );
        h.finish().await;
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn steers_that_miss_their_turn_open_the_next_one_together() {
    harn_clock::test_support::with_local_tasks(async {
        let mut h = DeliveryHarness::start().await;
        h.prompt(3, "hold", None);
        // `hold` never reaches a checkpoint, so neither steer can join it.
        h.prompt(4, "first steer", Some("steer"));
        h.prompt(5, "queued", Some("queue"));
        h.prompt(6, "second steer", Some("steer"));
        h.send(serde_json::json!({
            "jsonrpc": "2.0", "id": 7, "method": "session/pending_prompts",
            "params": {"sessionId": h.session_id},
        }));
        let (_, pending) = h.reply(7).await;
        let pending_ids: Vec<_> = pending["result"]["prompts"]
            .as_array()
            .expect("prompts")
            .iter()
            .map(|prompt| prompt["messageId"].clone())
            .collect();
        assert_eq!(pending_ids, ["m4", "m5", "m6"]);
        h.open_gate();

        let (hold_end_at, hold_end) = h.reply(3).await;
        assert_eq!(hold_end["result"]["stopReason"], "end_turn");
        let (steer_turn_at, _) = h.state_update(0, "running").await;
        let (steer_end_at, _) = h.state_update(0, "idle").await;
        let (queue_turn_at, _) = h.state_update(1, "running").await;
        let (queue_end_at, _) = h.state_update(1, "idle").await;
        let first = h.reply(4).await.0;
        let second = h.reply(6).await.0;
        let queued = h.reply(5).await.0;
        let m4 = h.user_message("m4").expect("first steer inserted");
        let m5 = h.user_message("m5").expect("queued prompt inserted");
        let m6 = h.user_message("m6").expect("second steer inserted");
        assert!(
            hold_end_at < steer_turn_at && steer_turn_at < m4 && m4 < first,
            "the first steer's receipt follows its insertion"
        );
        assert!(
            m6 < second && second < steer_end_at && first < steer_end_at,
            "both steers belong to the first owed turn"
        );
        assert!(
            steer_end_at < queue_turn_at
                && queue_turn_at < m5
                && m5 < queued
                && queued < queue_end_at,
            "the queued prompt's receipt follows its insertion"
        );
        assert!(m4 < m6, "steers keep arrival order");
        h.finish().await;
    })
    .await;
}
