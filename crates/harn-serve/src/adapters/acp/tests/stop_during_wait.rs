//! Stop during a blocking command wait: `session/cancel` must reach a turn that
//! is parked in `wait_command`, end it with a `user_cancelled` terminal, and
//! reap the child the wait was watching.
//!
//! The session thread runs both the turn and the router that reads the cancel,
//! so a wait that blocks that thread hides the stop until the wait times out.

use super::*;
use harn_clock::test_support::{recv_within, within, HANG_CEILING};

// A hung stop must fail its event watchdog before the command wait can expire.
const WAIT_TIMEOUT_MS: u64 = HANG_CEILING.as_millis() as u64 * 2;

struct BackgroundSessionCleanup(String);

impl Drop for BackgroundSessionCleanup {
    fn drop(&mut self) {
        harn_hostlib::tools::long_running::cancel_session_handles(&self.0);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn tool_call_cancel_reaches_its_background_command_after_the_call_returns() {
    let _guardian_args = harn_hostlib::process::owner_death::install_guardian_reexec_args([
        "--exact",
        "adapters::acp::tests::stop_controls::stop_controls_owner_death_guardian_fixture",
        "--nocapture",
    ]);
    let _environment = harn_vm::stdlib::process::declare_session_environment_if_absent(
        harn_vm::security::SessionEnvironment::inherited(),
    );
    let dir = tempfile::tempdir().expect("tempdir");
    let fifo = dir.path().join("command-input");
    assert!(std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap()
        .success());
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut server = AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx));
    let mut sessions = Vec::new();
    for id in [1, 2] {
        server.handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0", "id": id, "method": "session/new",
            "params": {"cwd": dir.path(), "environmentPolicy": {"kind": "isolated", "grants": []}},
        })).await;
        let created = recv_json(&mut rx).await;
        sessions.push(created["result"]["sessionId"].as_str().unwrap().to_string());
    }
    let _cleanup = BackgroundSessionCleanup(sessions[0].clone());
    let child = harn_hostlib::tools::long_running::spawn_long_running(
        "run",
        "cat".to_string(),
        vec![fifo.to_string_lossy().into_owned()],
        Some(dir.path().to_path_buf()),
        BTreeMap::new(),
        sessions[0].clone(),
    )
    .expect("spawn a FIFO-blocked command");
    assert!(
        is_running(child.pid),
        "the actual background command was reached"
    );
    let reaped = harn_hostlib::tools::long_running::register_result_notifier(&child.handle_id)
        .expect("live background command");
    let host_bridge = attach_test_host_bridge(&mut server, &sessions[0]);
    let registry = host_bridge.tool_call_cancellation_registry();
    let (in_flight, registration) = registry.register(sessions[0].clone(), "active-call", "run");
    for invalid in [serde_json::json!(""), serde_json::json!(7)] {
        server.handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0", "id": 10, "method": "session/cancel_tool_call",
            "params": {"sessionId": sessions[0], "toolCallId": "active-call", "handleId": invalid},
        })).await;
        let refused = recv_json(&mut rx).await;
        assert_eq!(refused["error"]["code"], -32602, "{refused}");
        assert!(
            !in_flight.is_cancelled(),
            "malformed command intent cannot fall through to tool-call cancellation"
        );
        assert!(is_running(child.pid));
    }
    drop(registration);
    for (id, session, expected) in [
        (3, &sessions[1], "not_found"),
        (4, &sessions[0], "cancelled"),
    ] {
        server.handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0", "id": id, "method": "session/cancel_tool_call",
            "params": {"sessionId": session, "toolCallId": "completed-run-call", "handleId": child.handle_id},
        })).await;
        let response = recv_json(&mut rx).await;
        assert_eq!(response["result"]["status"], expected, "{response}");
        assert_eq!(response["result"]["callId"], "completed-run-call");
        if expected == "not_found" {
            assert!(
                is_running(child.pid),
                "another session cannot cancel this command"
            );
        }
    }
    let terminal =
        tokio::task::spawn_blocking(move || recv_within("cancelled command reaped", &reaped))
            .await
            .expect("waiter task");
    assert_eq!(
        terminal
            .as_dict()
            .unwrap()
            .get("status")
            .map(harn_vm::VmValue::display)
            .as_deref(),
        Some("killed")
    );
    assert!(
        !is_running(child.pid),
        "the acknowledged cancellation reaps the real child"
    );
}

fn harn_string_literal(value: &str) -> String {
    format!("\"{}\"", harn_lexer::escape_string_literal(value))
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

/// Installs the runtime's own tools in each session VM, as the CLI does.
struct InstallHostlib;

#[async_trait::async_trait(?Send)]
impl AcpRuntimeConfigurator for InstallHostlib {
    async fn configure(
        &self,
        vm: &mut harn_vm::Vm,
        _source_path: Option<&std::path::Path>,
    ) -> Result<(), String> {
        let _ = harn_hostlib::install_default(vm);
        Ok(())
    }
}

/// Answer a `host/call` the way a host that owns no `tools.*` operation does,
/// so the call stays with the runtime's own tools.
fn reply_unavailable(
    request_tx: &mpsc::UnboundedSender<serde_json::Value>,
    message: &serde_json::Value,
) {
    request_tx
        .send(serde_json::json!({
            "jsonrpc": "2.0",
            "id": message["id"].clone(),
            "error": {"code": -32601, "message": "host capability unavailable"},
        }))
        .expect("send host/call error");
}

async fn end_parked_wait(cancel_session: bool) -> (serde_json::Value, String) {
    // Background spawns re-exec the test binary as their owner-death guardian.
    let _guardian_args = harn_hostlib::process::owner_death::install_guardian_reexec_args([
        "--exact",
        "adapters::acp::tests::stop_controls::stop_controls_owner_death_guardian_fixture",
        "--nocapture",
    ]);
    // The child is spawned directly, so the test declares its environment.
    let _environment = harn_vm::stdlib::process::declare_session_environment_if_absent(
        harn_vm::security::SessionEnvironment::inherited(),
    );
    harn_clock::test_support::with_local_tasks(async {
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir.path().join("progress.txt");
        let handle_path = dir.path().join("handle.txt");
        let fifo_path = dir.path().join("command-input");
        let pipeline_path = dir.path().join("wait-then-cancel.harn");
        let marker_literal = harn_string_literal(&marker.to_string_lossy());
        let handle_literal = harn_string_literal(&handle_path.to_string_lossy());
        let source = format!(
            r#"pipeline default(harness: Harness, task: unknown) {{
  const handle_id = harness.fs.read_text({handle_literal}).trim()
  harness.fs.append({marker_literal}, "WAITING\n")
  const settled = harness.tools.wait_command({{handle_id: handle_id, timeout_ms: {WAIT_TIMEOUT_MS}}})
  harness.fs.append({marker_literal}, "AFTER_WAIT:" + to_string(settled.status) + "\n")
}}
"#,
        );
        std::fs::write(&marker, "").expect("seed marker");
        std::fs::write(&pipeline_path, source).expect("write pipeline");
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&fifo_path)
                .status()
                .expect("create command FIFO")
                .success(),
            "create command FIFO"
        );

        let (request_tx, mut response_rx, server, session_id) = start_acp_code_session_with_config(
            AcpServerConfig::for_pipeline(pipeline_path.to_string_lossy().to_string())
                .with_runtime_configurator(Arc::new(InstallHostlib)),
            serde_json::json!(dir.path()),
        )
        .await;
        let _cleanup = BackgroundSessionCleanup(session_id.clone());
        // The session's own backgrounded child, keyed the way the command tool
        // keys it. Spawned here, on the thread that carries the guardian args.
        let child = harn_hostlib::tools::long_running::spawn_long_running(
            "run",
            "cat".to_string(),
            vec![fifo_path.to_string_lossy().into_owned()],
            Some(dir.path().to_path_buf()),
            std::collections::BTreeMap::new(),
            session_id.clone(),
        )
        .expect("spawn a FIFO-blocked child");
        let pid = child.pid;
        std::fs::write(&handle_path, &child.handle_id).expect("write handle id");
        assert!(is_running(pid), "background child should be running");
        let wait_registered =
            harn_hostlib::tools::long_running::register_wait_notifier(&child.handle_id)
                .expect("live handle");
        let reaped =
            harn_hostlib::tools::long_running::register_result_notifier(&child.handle_id)
                .expect("live handle");
        let mut wait_in_place = tokio::task::spawn_blocking(move || {
            recv_within("wait_command registration", &wait_registered);
        });
        request_tx
            .send(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "session/prompt",
                "params": {
                    "sessionId": session_id.clone(),
                    "prompt": [{"type": "text", "text": "wait"}],
                },
            }))
            .expect("send session/prompt");

        // Serve the host until the wait has registered for its result, so the
        // stop arrives while the turn is parked inside the wait.
        let mut early = serde_json::Value::Null;
        within("turn parked in wait_command", async {
            loop {
                tokio::select! {
                    registered = &mut wait_in_place => {
                        registered.expect("wait notifier task");
                        assert!(early.is_null(), "prompt ended before its wait: {early}");
                        break;
                    }
                    message = recv_json(&mut response_rx) => {
                        if message["method"] == "host/capabilities" {
                            reply_empty_result(&request_tx, &message);
                        } else if message["method"] == "host/call" {
                            reply_unavailable(&request_tx, &message);
                        } else if message["id"] == 3 {
                            early = message.clone();
                        }
                    }
                }
            }
        })
        .await;

        if cancel_session {
            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "session/cancel",
                    "params": {"sessionId": session_id.clone()},
                }))
                .expect("send session/cancel");
        } else {
            assert!(harn_hostlib::tools::long_running::cancel_handle(
                &child.handle_id
            ));
        }

        let result = within("prompt terminal after ending its wait", async {
            loop {
                let message = recv_json(&mut response_rx).await;
                if message["method"] == "host/capabilities" {
                    reply_empty_result(&request_tx, &message);
                } else if message["method"] == "host/call" {
                    reply_unavailable(&request_tx, &message);
                } else if message["id"] == 3 {
                    return message["result"].clone();
                }
            }
        })
        .await;

        // The handle publishes its terminal result once its waiter has reaped
        // the killed child.
        within(
            "waited child reaped",
            tokio::task::spawn_blocking(move || recv_within("child terminal result", &reaped)),
        )
        .await
        .expect("reap notifier task");
        let child_alive = is_running(pid);
        let marker_text = std::fs::read_to_string(&marker).unwrap_or_default();

        drop(request_tx);
        within("ACP session teardown", server)
            .await
            .expect("ACP server task");
        assert!(marker_text.starts_with("WAITING\n"), "{marker_text}");
        // The cancel reaps the child, which wakes the wait with that terminal
        // result; a wait that sat out its budget would report `running`.
        assert!(
            !marker_text.contains("AFTER_WAIT:running"),
            "the wait ran out its budget instead of ending on the stop: {marker_text}"
        );
        assert!(!child_alive, "the waited child (pid {pid}) must be reaped");
        (result, marker_text)
    })
    .await
}

#[tokio::test(flavor = "current_thread")]
async fn session_cancel_ends_a_turn_parked_in_wait_command() {
    let (result, _) = end_parked_wait(true).await;
    assert_eq!(result["stopReason"], "cancelled", "got {result}");
    assert_eq!(
        result["_meta"]["harn"]["terminal"]["kind"], "user_cancelled",
        "a stopped wait must seal a user_cancelled terminal; got {result}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn ending_the_waited_command_does_not_cancel_the_turn() {
    let (result, marker) = end_parked_wait(false).await;
    assert_eq!(result["stopReason"], "end_turn", "got {result}");
    assert_ne!(
        result["_meta"]["harn"]["terminal"]["kind"],
        "user_cancelled"
    );
    assert!(marker.contains("AFTER_WAIT:"), "{marker}");
}
