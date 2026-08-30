//! Stop is a control event: `session/cancel` ends the agent loop, and a cancel
//! the server cannot honor says so instead of vanishing.
//!
//! Deterministic and model-free. Each test drives a real ACP channel session,
//! runs a mocked multi-iteration agent loop, sends the notification form of
//! `session/cancel` mid-loop, and counts what still ran afterwards.
//!
//! The negative control is what keeps the two positive tests honest: it uses
//! the identical harness but names a session id the server never registered,
//! and asserts the loop DOES run on. Without it, a harness that silently failed
//! to start a loop at all would let "nothing ran after the cancel" pass
//! vacuously.

use super::*;

const TICK_SLEEP_MS: u64 = 120;
const MAX_ITERATIONS: usize = 30;

fn count_ticks(path: &std::path::Path) -> usize {
    std::fs::read_to_string(path)
        .map(|text| text.lines().filter(|line| !line.is_empty()).count())
        .unwrap_or(0)
}

#[tokio::test(flavor = "current_thread")]
async fn acp_session_cancel_notification_stops_agent_loop() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let tick_path = dir.path().join("ticks.txt");
            std::fs::write(&tick_path, "").expect("seed tick file");
            let pipeline_path = dir.path().join("cancel-loop.harn");

            let mut mocks = String::new();
            for index in 0..MAX_ITERATIONS {
                mocks.push_str(&format!(
                    "  harness.llm.mock_enqueue({{text: \"\", tool_calls: \
                     [{{id: \"c{index}\", name: \"tick\", arguments: {{n: {index}}}}}]}})\n"
                ));
            }

            let source = format!(
                r#"import {{ agent_loop }} from "std/agent/loop"

fn tick_tools(harness: Harness) {{
  let tools = tool_registry()
  tools = tool_define(
    tools,
    "tick",
    "Record one tick and wait",
    {{
      handler: {{ args ->
        harness.fs.append("{tick}", "tick\n")
        harness.clock.sleep_ms({sleep})
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
  harness.llm.mock_clear()
{mocks}
  const result = agent_loop(harness, "tick until told to stop", nil, {{
    provider: "mock",
    tools: tick_tools(harness),
    tool_format: "native",
    loop_until_done: true,
    max_iterations: {MAX_ITERATIONS},
    done_judge: nil,
  }})
  harness.fs.append("{tick}", "STATUS:" + to_string(result.status) + "\n")
}}
"#,
                tick = tick_path.to_string_lossy(),
                sleep = TICK_SLEEP_MS,
            );
            std::fs::write(&pipeline_path, source).expect("write cancel-loop pipeline");

            let (request_tx, mut response_rx, server, session_id) =
                start_acp_code_session_with_config(
                    AcpServerConfig::for_pipeline(pipeline_path.to_string_lossy().to_string()),
                    serde_json::json!(dir.path()),
                )
                .await;

            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 3,
                    "method": "session/prompt",
                    "params": {
                        "sessionId": session_id.clone(),
                        "prompt": [{"type": "text", "text": "tick"}],
                    },
                }))
                .expect("send session/prompt");

            // Wait until the loop is demonstrably running (>= 2 ticks), then
            // send the NOTIFICATION form of session/cancel (no `id`).
            let mut ticks_at_cancel = 0usize;
            let mut prompt_result_early = serde_json::Value::Null;
            for _ in 0..400 {
                ticks_at_cancel = count_ticks(&tick_path);
                if ticks_at_cancel >= 2 {
                    break;
                }
                // Drain (and answer) anything the server needs while the loop
                // spins up, so a `host/capabilities` round trip cannot stall it.
                while let Ok(line) = response_rx.try_recv() {
                    let message: serde_json::Value =
                        serde_json::from_str(&line).expect("ACP JSON line");
                    if message["method"] == "host/capabilities" {
                        request_tx
                            .send(serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": message["id"].clone(),
                                "result": {},
                            }))
                            .expect("send host capabilities response");
                    } else if message["id"] == 3 {
                        prompt_result_early = message["result"].clone();
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            assert!(
                prompt_result_early.is_null(),
                "prompt finished before the cancel could be sent: {prompt_result_early}"
            );
            assert!(
                ticks_at_cancel >= 2,
                "agent loop never reached 2 ticks; saw {ticks_at_cancel}"
            );

            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "session/cancel",
                    "params": {"sessionId": session_id.clone()},
                }))
                .expect("send session/cancel notification");

            let mut result = serde_json::Value::Null;
            for _ in 0..256 {
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
                    result = message["result"].clone();
                    break;
                }
            }

            let ticks_total = count_ticks(&tick_path);
            let ticks_after_cancel = ticks_total.saturating_sub(ticks_at_cancel);

            drop(request_tx);
            let _ = server.await;

            assert!(
                ticks_after_cancel <= 1,
                "session/cancel did not stop the agent loop: {ticks_after_cancel} tool \
                 iterations ran after the cancel notification (at_cancel={ticks_at_cancel}, \
                 total={ticks_total}); prompt result = {result}"
            );
            assert_eq!(
                result["stopReason"], "cancelled",
                "expected ACP stopReason=cancelled; got {result}"
            );
            // harn#7581: a cancel is a terminal outcome like any other, so the
            // result carries the same typed `_meta.harn.terminal` every
            // non-cancelled path carries. Reporting only `stopReason` left
            // every reader of the typed terminal seeing a turn with none.
            assert_eq!(
                result["_meta"]["harn"]["terminal"]["kind"], "user_cancelled",
                "a cancelled turn must seal a user_cancelled terminal; got {result}"
            );
            assert_eq!(
                result["_meta"]["harn"]["terminal"]["owner"], "user",
                "a user cancel is owned by the user; got {result}"
            );
        })
        .await;
}

/// Variant 2: the tool is NOT registered on the Harn side, so dispatch falls
/// through to `bridge.call("builtin_call", ...)` — the shape Burin uses, where
/// every tool is a host builtin answered over JSON-RPC. The test host answers
/// each `builtin_call` after a delay, and sends the `session/cancel`
/// notification while one is outstanding.
#[tokio::test(flavor = "current_thread")]
async fn acp_session_cancel_notification_stops_bridge_routed_agent_loop() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let pipeline_path = dir.path().join("cancel-bridge-loop.harn");

            let mut mocks = String::new();
            for index in 0..MAX_ITERATIONS {
                mocks.push_str(&format!(
                    "  harness.llm.mock_enqueue({{text: \"\", tool_calls: \
                     [{{id: \"c{index}\", name: \"tick\", arguments: {{n: {index}}}}}]}})\n"
                ));
            }

            let source = format!(
                r#"import {{ agent_loop }} from "std/agent/loop"

pipeline default(harness: Harness, task: unknown) {{
  harness.llm.mock_clear()
{mocks}
  const result = agent_loop(harness, "tick until told to stop", nil, {{
    provider: "mock",
    tool_format: "native",
    loop_until_done: true,
    max_iterations: {MAX_ITERATIONS},
    done_judge: nil,
  }})
  harness.stdio.println("LOOP_STATUS:" + to_string(result.status))
}}
"#,
            );
            std::fs::write(&pipeline_path, source).expect("write bridge pipeline");

            let (request_tx, mut response_rx, server, session_id) =
                start_acp_code_session_with_config(
                    AcpServerConfig::for_pipeline(pipeline_path.to_string_lossy().to_string()),
                    serde_json::json!(dir.path()),
                )
                .await;

            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 3,
                    "method": "session/prompt",
                    "params": {
                        "sessionId": session_id.clone(),
                        "prompt": [{"type": "text", "text": "tick"}],
                    },
                }))
                .expect("send session/prompt");

            let mut builtin_calls = 0usize;
            let mut builtin_calls_at_cancel: Option<usize> = None;
            let mut iteration_starts = 0usize;
            let mut iteration_starts_at_cancel: Option<usize> = None;
            let mut llm_call_starts = 0usize;
            let mut llm_call_starts_at_cancel: Option<usize> = None;
            let mut result = serde_json::Value::Null;

            for _ in 0..4096 {
                let message = match tokio::time::timeout(
                    std::time::Duration::from_secs(20),
                    response_rx.recv(),
                )
                .await
                {
                    Ok(Some(line)) => {
                        serde_json::from_str::<serde_json::Value>(&line).expect("ACP JSON line")
                    }
                    _ => break,
                };
                let method = message["method"].as_str().unwrap_or_default();
                if method == "host/capabilities" {
                    request_tx
                        .send(serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": message["id"].clone(),
                            "result": {},
                        }))
                        .expect("send host capabilities response");
                    continue;
                }
                if method == "builtin_call" {
                    builtin_calls += 1;
                    // Answer slowly so a cancel can land mid-call.
                    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                    request_tx
                        .send(serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": message["id"].clone(),
                            "result": "ticked",
                        }))
                        .expect("send builtin_call response");
                    if builtin_calls == 2 && builtin_calls_at_cancel.is_none() {
                        builtin_calls_at_cancel = Some(builtin_calls);
                        iteration_starts_at_cancel = Some(iteration_starts);
                        llm_call_starts_at_cancel = Some(llm_call_starts);
                        request_tx
                            .send(serde_json::json!({
                                "jsonrpc": "2.0",
                                "method": "session/cancel",
                                "params": {"sessionId": session_id.clone()},
                            }))
                            .expect("send session/cancel notification");
                    }
                    continue;
                }
                if method == "_harn/agentEvent" {
                    if message["params"]["kind"] == "iteration_start" {
                        iteration_starts += 1;
                    }
                    if message["params"]["checkpoint"]["kind"] == "llm_call_start" {
                        llm_call_starts += 1;
                    }
                    continue;
                }
                if message["id"] == 3 {
                    result = message["result"].clone();
                    break;
                }
            }

            let after_iterations =
                iteration_starts.saturating_sub(iteration_starts_at_cancel.unwrap_or(0));
            let after_llm = llm_call_starts.saturating_sub(llm_call_starts_at_cancel.unwrap_or(0));
            let after_tools = builtin_calls.saturating_sub(builtin_calls_at_cancel.unwrap_or(0));

            drop(request_tx);
            let _ = server.await;

            assert!(
                builtin_calls_at_cancel.is_some(),
                "never reached 2 bridge-routed tool calls"
            );
            assert!(
                after_iterations <= 1,
                "session/cancel did not stop the bridge-routed agent loop: \
                 {after_iterations} more iteration_start, {after_llm} more llm_call_start, \
                 {after_tools} more tool calls; result = {result}"
            );
        })
        .await;
}

/// NEGATIVE CONTROL for the two tests above: identical harness, but the
/// `session/cancel` notification carries a session id the server has never
/// registered. `preempt_session_interruption` still returns `true` (the frame
/// is swallowed, never routed, never answered), `mark_cancelled_session`
/// returns `false`, and the agent loop runs to its iteration cap. If this
/// control ever stops running past the cancel, the two tests above are passing
/// vacuously.
#[tokio::test(flavor = "current_thread")]
async fn control_unknown_session_id_cancel_does_not_stop_the_live_loop() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let pipeline_path = dir.path().join("control-loop.harn");

            let mut mocks = String::new();
            for index in 0..6 {
                mocks.push_str(&format!(
                    "  harness.llm.mock_enqueue({{text: \"\", tool_calls: \
                     [{{id: \"c{index}\", name: \"tick\", arguments: {{n: {index}}}}}]}})\n"
                ));
            }

            let source = format!(
                r#"import {{ agent_loop }} from "std/agent/loop"

pipeline default(harness: Harness, task: unknown) {{
  harness.llm.mock_clear()
{mocks}
  agent_loop(harness, "tick until told to stop", nil, {{
    provider: "mock",
    tool_format: "native",
    loop_until_done: true,
    max_iterations: 6,
    done_judge: nil,
  }})
}}
"#
            );
            std::fs::write(&pipeline_path, source).expect("write control pipeline");

            let (request_tx, mut response_rx, server, session_id) =
                start_acp_code_session_with_config(
                    AcpServerConfig::for_pipeline(pipeline_path.to_string_lossy().to_string()),
                    serde_json::json!(dir.path()),
                )
                .await;

            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 3,
                    "method": "session/prompt",
                    "params": {
                        "sessionId": session_id.clone(),
                        "prompt": [{"type": "text", "text": "tick"}],
                    },
                }))
                .expect("send session/prompt");

            let mut builtin_calls = 0usize;
            let mut builtin_calls_at_cancel: Option<usize> = None;
            let mut result = serde_json::Value::Null;
            for _ in 0..4096 {
                let message = match tokio::time::timeout(
                    std::time::Duration::from_secs(20),
                    response_rx.recv(),
                )
                .await
                {
                    Ok(Some(line)) => {
                        serde_json::from_str::<serde_json::Value>(&line).expect("ACP JSON line")
                    }
                    _ => break,
                };
                let method = message["method"].as_str().unwrap_or_default();
                if method == "host/capabilities" {
                    request_tx
                        .send(serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": message["id"].clone(),
                            "result": {},
                        }))
                        .expect("send host capabilities response");
                    continue;
                }
                if method == "builtin_call" {
                    builtin_calls += 1;
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    request_tx
                        .send(serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": message["id"].clone(),
                            "result": "ticked",
                        }))
                        .expect("send builtin_call response");
                    if builtin_calls == 2 && builtin_calls_at_cancel.is_none() {
                        builtin_calls_at_cancel = Some(builtin_calls);
                        request_tx
                            .send(serde_json::json!({
                                "jsonrpc": "2.0",
                                "method": "session/cancel",
                                "params": {"sessionId": "not-a-registered-session"},
                            }))
                            .expect("send session/cancel notification");
                    }
                    continue;
                }
                if message["id"] == 3 {
                    result = message["result"].clone();
                    break;
                }
            }

            let after = builtin_calls.saturating_sub(builtin_calls_at_cancel.unwrap_or(0));

            drop(request_tx);
            let _ = server.await;

            assert!(
                after >= 2,
                "negative control did not run past the cancel; the positive tests may be \
                 passing vacuously (after={after}, total={builtin_calls}, result={result})"
            );
            assert_ne!(
                result["stopReason"], "cancelled",
                "an unknown-session cancel must not report the live session as cancelled"
            );
        })
        .await;
}

/// The guardian a backgrounded command handle re-execs into.
///
/// `spawn_long_running` re-runs `current_exe` with the guardian argv marker, so
/// inside a test binary that target has to be a test that runs the guardian —
/// otherwise every background spawn dies with "guardian exited before payload
/// startup" and the kill test below would fail for a reason that has nothing to
/// do with cancel.
#[cfg(feature = "hostlib")]
#[test]
fn stop_controls_owner_death_guardian_fixture() {
    if !harn_hostlib::process::owner_death::guardian_requested() {
        return;
    }
    harn_hostlib::process::owner_death::run_guardian_from_pipe().expect("run owner-death guardian");
}

/// An accepted cancel must reach the processes the session started, not just
/// its loop — and must stop at that session's own children.
///
/// Backgrounded command handles outlive the tool call that started them by
/// design, so unwinding the agent loop never reaches them. Before the fix a
/// stop left them running, and in a long-lived host (which does not exit and so
/// never triggers the owner-death guardian) their death depended on the model
/// choosing to call the kill tool.
///
/// The second session is the scope control: without it this test would pass
/// just as happily if a cancel killed every background child on the host.
#[cfg(feature = "hostlib")]
#[test]
fn accepted_cancel_kills_only_the_cancelled_sessions_background_children() {
    use std::collections::BTreeMap;
    use std::time::{Duration, Instant};

    let _guardian_args = harn_hostlib::process::owner_death::install_guardian_reexec_args([
        "--exact",
        "adapters::acp::tests::stop_controls::stop_controls_owner_death_guardian_fixture",
        "--nocapture",
    ]);

    fn spawn_sleeper(session_id: &str) -> u32 {
        harn_hostlib::tools::long_running::spawn_long_running(
            "run",
            "sleep".to_string(),
            vec!["300".to_string()],
            Some(std::env::temp_dir()),
            BTreeMap::new(),
            session_id.to_string(),
        )
        .expect("spawn a backgrounded sleep")
        .pid
    }

    // A pid is gone once it is neither live nor a reaped-but-listed zombie.
    fn is_running(pid: u32) -> bool {
        let output = std::process::Command::new("ps")
            .args(["-o", "stat=", "-p", &pid.to_string()])
            .output()
            .expect("ps");
        let stat = String::from_utf8_lossy(&output.stdout);
        let stat = stat.trim();
        !stat.is_empty() && !stat.starts_with('Z')
    }

    let cancelled_session = format!("cancel-me-{}", std::process::id());
    let bystander_session = format!("leave-me-{}", std::process::id());
    let cancelled_pid = spawn_sleeper(&cancelled_session);
    let bystander_pid = spawn_sleeper(&bystander_session);
    assert!(is_running(cancelled_pid), "target child should start");
    assert!(is_running(bystander_pid), "bystander child should start");

    let cancellations: Arc<std::sync::Mutex<HashMap<String, SessionCancellation>>> =
        Arc::new(std::sync::Mutex::new(HashMap::new()));
    for session in [&cancelled_session, &bystander_session] {
        cancellations
            .lock()
            .unwrap()
            .insert(session.clone(), SessionCancellation::default());
    }

    // The notification form, exactly as a host sends it.
    let consumed = preempt_session_interruption(
        &cancellations,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "method": "session/cancel",
            "params": {"sessionId": cancelled_session},
        }),
    );
    assert!(consumed, "a cancel that lands is consumed by the router");

    // Bounded poll: the kill is a signal, so a single immediate reading races
    // the child's teardown and would report a working stop as a leak.
    let deadline = Instant::now() + Duration::from_secs(2);
    while is_running(cancelled_pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(
        !is_running(cancelled_pid),
        "session/cancel must kill the session's backgrounded child within 2s \
         (pid {cancelled_pid} still running)"
    );
    assert!(
        is_running(bystander_pid),
        "a cancel must not reach another session's children (pid {bystander_pid} died)"
    );

    harn_hostlib::tools::long_running::cancel_session_handles(&bystander_session);
}

/// The identity `cancel_session_handles` depends on, asserted rather than assumed.
///
/// Backgrounded command handles are keyed by `current_agent_session_id()`, while
/// a cancel arrives naming the ACP session id. `handle_session_prompt` registers
/// the cancellation under the ACP session id and then enters the agent-session
/// guard with that same id, which is what makes
/// `cancel_session_command_handles(acp_session_id)` address the session's real
/// children. If those two id spaces ever diverge, the call still "succeeds" and
/// silently matches nothing — a stop that reads as working and kills nobody.
#[test]
fn agent_session_guard_binds_the_id_backgrounded_handles_are_keyed_by() {
    let session_id = format!("acp-identity-{}", std::process::id());
    let _guard = harn_vm::agent_sessions::enter_current_session(session_id.clone());
    assert_eq!(
        harn_vm::current_agent_session_id().as_deref(),
        Some(session_id.as_str()),
        "handles keyed by current_agent_session_id must be addressable by the ACP session id"
    );
}
