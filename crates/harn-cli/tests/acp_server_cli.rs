// Portable across Unix and Windows: this suite drives `harn serve acp` over
// piped stdio and shuts the child down by closing stdin (EOF), so it does not
// rely on POSIX signals or platform-specific tooling.

mod test_util;

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::Stdio;
use std::sync::{Mutex, MutexGuard, OnceLock};

use serde_json::{json, Value as JsonValue};
use tempfile::TempDir;
use test_util::process::harn_e2e_command;

static ACP_CLI_TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn lock_acp_cli_tests() -> MutexGuard<'static, ()> {
    ACP_CLI_TEST_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap()
}

fn write_file(dir: &Path, relative: &str, contents: &str) {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

fn write_fixture(temp: &TempDir) {
    write_file(
        temp.path(),
        "acp_fixture.harn",
        r#"
pub pipeline main() {
  let sid = agent_session_current_id()
  guard sid != nil else { throw "ACP prompt installs the current session id" }
  if prompt != "snapshot" {
    agent_session_inject(sid, {role: "user", content: prompt})
  }
  let snap = agent_session_snapshot(sid)
  println(
    json_stringify({
      session_id: sid,
      len: len(snap["messages"]),
      parent_id: snap["parent_id"],
      branched_at: snap["branched_at_event_index"],
      messages: snap["messages"],
    }),
  )
}
"#,
    );
}

fn send_request(
    stdin: &mut impl Write,
    stdout: &mut BufReader<impl std::io::Read>,
    request: JsonValue,
) -> (Vec<JsonValue>, JsonValue) {
    let request_id = request["id"].clone();
    writeln!(stdin, "{}", serde_json::to_string(&request).unwrap()).unwrap();
    stdin.flush().unwrap();

    let mut notifications = Vec::new();
    loop {
        let mut line = String::new();
        let read = stdout.read_line(&mut line).unwrap();
        assert!(read > 0, "ACP server closed stdout before responding");
        let message: JsonValue = serde_json::from_str(line.trim()).unwrap();
        if message.get("method").is_some() && message.get("id").is_some() {
            let method = message["method"].as_str().unwrap_or_default();
            let result = match method {
                "host/capabilities" => json!({}),
                other => panic!("unexpected ACP server request: {other}"),
            };
            writeln!(
                stdin,
                "{}",
                serde_json::to_string(&json!({
                    "jsonrpc": "2.0",
                    "id": message["id"].clone(),
                    "result": result,
                }))
                .unwrap()
            )
            .unwrap();
            stdin.flush().unwrap();
            continue;
        }
        if message.get("id") == Some(&request_id) {
            return (notifications, message);
        }
        notifications.push(message);
    }
}

fn latest_prompt_summary(notifications: &[JsonValue], session_id: &str) -> JsonValue {
    notifications
        .iter()
        .rev()
        .find_map(|message| {
            if message["method"] != "session/update" {
                return None;
            }
            if message["params"]["sessionId"] != session_id {
                return None;
            }
            if message["params"]["update"]["sessionUpdate"] != "agent_message_chunk" {
                return None;
            }
            let text = message["params"]["update"]["content"]["text"].as_str()?;
            serde_json::from_str(text.trim()).ok()
        })
        .unwrap_or_else(|| panic!("no prompt summary found for session {session_id}"))
}

#[ignore = "binary surface — moves to slow E2E/smoke job (issue #1069)"]
#[test]
fn acp_session_fork_branches_runtime_state_and_dispatches_independently() {
    let _guard = lock_acp_cli_tests();
    let temp = TempDir::new().unwrap();
    write_fixture(&temp);

    let mut child = harn_e2e_command()
        .current_dir(temp.path())
        .arg("serve")
        .arg("acp")
        .arg("acp_fixture.harn")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    let (_, init) = send_request(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {}
        }),
    );
    assert_eq!(init["result"]["agentCapabilities"]["loadSession"], true);
    assert_eq!(init["result"]["authMethods"], json!([]));
    assert_eq!(
        init["result"]["agentCapabilities"]["mcpCapabilities"],
        json!({
            "http": true,
            "sse": true,
        })
    );
    assert_eq!(
        init["result"]["agentCapabilities"]["sessionCapabilities"],
        json!({
            "close": {},
            "list": {},
            "resume": {},
        })
    );
    assert!(init["result"]["agentCapabilities"]["sessionCapabilities"]
        .get("fork")
        .is_none());

    let (_, created) = send_request(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "session/new",
            "params": {
                "cwd": temp.path(),
            }
        }),
    );
    let session_id = created["result"]["sessionId"].as_str().unwrap().to_string();

    let (alpha_notifications, alpha_response) = send_request(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "session/prompt",
            "params": {
                "sessionId": session_id,
                "prompt": [{ "type": "text", "text": "alpha" }]
            }
        }),
    );
    assert_eq!(alpha_response["result"]["stopReason"], "end_turn");
    let alpha_summary = latest_prompt_summary(&alpha_notifications, &session_id);
    assert_eq!(alpha_summary["len"], 1);

    let (beta_notifications, beta_response) = send_request(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "session/prompt",
            "params": {
                "sessionId": session_id,
                "prompt": [{ "type": "text", "text": "beta" }]
            }
        }),
    );
    assert_eq!(beta_response["result"]["stopReason"], "end_turn");
    let beta_summary = latest_prompt_summary(&beta_notifications, &session_id);
    assert_eq!(beta_summary["len"], 2);
    assert_eq!(beta_summary["messages"][0]["content"], "alpha");
    assert_eq!(beta_summary["messages"][1]["content"], "beta");

    let branch_id = "branch-left";
    let (fork_notifications, fork_response) = send_request(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "session/fork",
            "params": {
                "session_id": session_id,
                "keep_first": 1,
                "id": branch_id,
                "branch_name": "left"
            }
        }),
    );
    assert_eq!(fork_response["result"]["sessionId"], branch_id);
    assert_eq!(fork_response["result"]["state"], "forked");
    assert_eq!(fork_response["result"]["parent_id"], session_id);
    assert_eq!(fork_response["result"]["branched_at"], 1);
    let session_info_update = fork_notifications
        .iter()
        .find(|message| {
            message["method"] == "session/update"
                && message["params"]["sessionId"] == branch_id
                && message["params"]["update"]["sessionUpdate"] == "session_info_update"
        })
        .unwrap_or_else(|| panic!("missing session_info_update for forked session"));
    assert_eq!(
        session_info_update["params"]["update"]["_meta"]["state"],
        "forked"
    );
    assert_eq!(
        session_info_update["params"]["update"]["_meta"]["parent_id"],
        session_id
    );
    assert_eq!(
        session_info_update["params"]["update"]["_meta"]["branched_at"],
        1
    );
    assert_eq!(
        session_info_update["params"]["update"]["_meta"]["branch_name"],
        "left"
    );

    let (list_notifications, listed) = send_request(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc": "2.0",
            "id": 6,
            "method": "session/list",
            "params": {}
        }),
    );
    assert!(list_notifications.is_empty());
    let listed_branch = listed["result"]["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["sessionId"] == branch_id)
        .expect("forked session appears in session/list");
    assert_eq!(listed_branch["title"], "left");
    assert_eq!(listed_branch["_meta"]["state"], "forked");
    assert_eq!(listed_branch["_meta"]["parent_id"], session_id);

    let (child_notifications, child_response) = send_request(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "session/prompt",
            "params": {
                "sessionId": branch_id,
                "prompt": [{ "type": "text", "text": "child" }]
            }
        }),
    );
    assert_eq!(child_response["result"]["stopReason"], "end_turn");
    assert!(child_notifications.iter().all(|message| {
        message["method"] != "session/update" || message["params"]["sessionId"] == branch_id
    }));
    let child_summary = latest_prompt_summary(&child_notifications, branch_id);
    assert_eq!(child_summary["len"], 2);
    assert_eq!(child_summary["parent_id"], session_id);
    assert_eq!(child_summary["branched_at"], 1);
    assert_eq!(child_summary["messages"][0]["content"], "alpha");
    assert_eq!(child_summary["messages"][1]["content"], "child");

    let (parent_notifications, parent_response) = send_request(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc": "2.0",
            "id": 8,
            "method": "session/prompt",
            "params": {
                "sessionId": session_id,
                "prompt": [{ "type": "text", "text": "snapshot" }]
            }
        }),
    );
    assert_eq!(parent_response["result"]["stopReason"], "end_turn");
    let parent_summary = latest_prompt_summary(&parent_notifications, &session_id);
    assert_eq!(parent_summary["len"], 2);
    assert_eq!(parent_summary["messages"][0]["content"], "alpha");
    assert_eq!(parent_summary["messages"][1]["content"], "beta");

    drop(stdin);
    let status = child.wait().unwrap();
    assert!(status.success(), "status={status}");
}

/// Round-trip the v1 mid-session model swap: pin a model via
/// `session/set_config_option(configId="model")`, observe the
/// `config_option_update` notification, and verify the next
/// `session/prompt` sees the pin reflected in `agent_session_snapshot`.
/// Locks the wire contract from #1721 against future regressions.
#[ignore = "binary surface — moves to slow E2E/smoke job (issue #1069)"]
#[test]
fn acp_session_set_config_option_model_pins_for_next_prompt() {
    let _guard = lock_acp_cli_tests();
    let temp = TempDir::new().unwrap();
    write_file(
        temp.path(),
        "pin_fixture.harn",
        r#"
pub pipeline main() {
  let sid = agent_session_current_id()
  guard sid != nil else { throw "ACP prompt installs the current session id" }
  let snap = agent_session_snapshot(sid)
  println(
    json_stringify({
      session_id: sid,
      pinned_model: snap["pinned_model"],
    }),
  )
}
"#,
    );

    let mut child = harn_e2e_command()
        .current_dir(temp.path())
        .arg("serve")
        .arg("acp")
        .arg("pin_fixture.harn")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    let (_, _init) = send_request(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {},
        }),
    );

    let (_, created) = send_request(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "session/new",
            "params": {"cwd": temp.path()},
        }),
    );
    let session_id = created["result"]["sessionId"].as_str().unwrap().to_string();
    // Freshly created sessions have no model pinned — the `model`
    // config option must advertise the inherit sentinel.
    let model_option = created["result"]["configOptions"]
        .as_array()
        .expect("configOptions array")
        .iter()
        .find(|entry| entry["id"] == "model")
        .expect("model config option in session/new response");
    assert_eq!(model_option["currentValue"], "@inherit");

    let (_pin_notifications, pin_ack) = send_request(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "session/set_config_option",
            "params": {
                "sessionId": session_id,
                "configId": "model",
                "value": "claude-sonnet-4-6",
            },
        }),
    );
    let ack_model = pin_ack["result"]["configOptions"]
        .as_array()
        .expect("configOptions array")
        .iter()
        .find(|entry| entry["id"] == "model")
        .expect("model config option in ack");
    assert_eq!(ack_model["currentValue"], "claude-sonnet-4-6");

    // The `config_option_update` notification is emitted *after* the
    // ack (matching the `session/set_mode` convention), so it lands in
    // the notification buffer of the *next* request rather than the
    // one that triggered it.
    let (prompt_notifications, prompt_response) = send_request(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "session/prompt",
            "params": {
                "sessionId": session_id,
                "prompt": [{"type": "text", "text": "irrelevant"}],
            },
        }),
    );
    assert_eq!(prompt_response["result"]["stopReason"], "end_turn");
    let summary = latest_prompt_summary(&prompt_notifications, &session_id);
    assert_eq!(summary["pinned_model"], "claude-sonnet-4-6");

    let saw_update = prompt_notifications.iter().any(|message| {
        message["method"] == "session/update"
            && message["params"]["sessionId"] == session_id
            && message["params"]["update"]["sessionUpdate"] == "config_option_update"
            && message["params"]["update"]["configOptions"]
                .as_array()
                .map(|entries| {
                    entries.iter().any(|entry| {
                        entry["id"] == "model" && entry["currentValue"] == "claude-sonnet-4-6"
                    })
                })
                .unwrap_or(false)
    });
    assert!(
        saw_update,
        "set_config_option(model) must emit config_option_update reflecting the pin"
    );

    drop(stdin);
    let status = child.wait().unwrap();
    assert!(status.success(), "status={status}");
}

#[ignore = "binary surface — moves to slow E2E/smoke job (issue #1069)"]
#[test]
fn acp_session_truncate_mutates_runtime_state_in_place() {
    let _guard = lock_acp_cli_tests();
    let temp = TempDir::new().unwrap();
    write_fixture(&temp);

    let mut child = harn_e2e_command()
        .current_dir(temp.path())
        .arg("serve")
        .arg("acp")
        .arg("acp_fixture.harn")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    let (_, created) = send_request(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "session/new",
            "params": {
                "cwd": temp.path(),
            }
        }),
    );
    let session_id = created["result"]["sessionId"].as_str().unwrap().to_string();

    let (alpha_notifications, alpha_response) = send_request(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "session/prompt",
            "params": {
                "sessionId": session_id,
                "prompt": [{ "type": "text", "text": "alpha" }]
            }
        }),
    );
    assert_eq!(alpha_response["result"]["stopReason"], "end_turn");
    assert_eq!(
        latest_prompt_summary(&alpha_notifications, &session_id)["len"],
        1
    );

    let (beta_notifications, beta_response) = send_request(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "session/prompt",
            "params": {
                "sessionId": session_id,
                "prompt": [{ "type": "text", "text": "beta" }]
            }
        }),
    );
    assert_eq!(beta_response["result"]["stopReason"], "end_turn");
    assert_eq!(
        latest_prompt_summary(&beta_notifications, &session_id)["len"],
        2
    );

    let (truncate_notifications, truncate_response) = send_request(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "session/truncate",
            "params": {
                "sessionId": session_id,
                "keepFirst": 1,
                "reason": "user_edit"
            }
        }),
    );
    assert_eq!(truncate_response["result"]["sessionId"], session_id);
    assert_eq!(truncate_response["result"]["keptTurnCount"], 1);
    assert_eq!(truncate_response["result"]["removedTurnCount"], 1);
    assert!(truncate_response["result"]["newTipTurnId"].is_string());
    let truncate_update = truncate_notifications
        .iter()
        .find(|message| {
            message["method"] == "session/update"
                && message["params"]["sessionId"] == session_id
                && message["params"]["update"]["sessionUpdate"] == "session_truncated"
        })
        .unwrap_or_else(|| panic!("missing session_truncated update"));
    assert_eq!(truncate_update["params"]["update"]["reason"], "user_edit");

    let (snapshot_notifications, snapshot_response) = send_request(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "session/prompt",
            "params": {
                "sessionId": session_id,
                "prompt": [{ "type": "text", "text": "snapshot" }]
            }
        }),
    );
    assert_eq!(snapshot_response["result"]["stopReason"], "end_turn");
    let snapshot = latest_prompt_summary(&snapshot_notifications, &session_id);
    assert_eq!(snapshot["len"], 1);
    assert_eq!(snapshot["messages"][0]["content"], "alpha");

    drop(stdin);
    let status = child.wait().unwrap();
    assert!(status.success(), "status={status}");
}

#[ignore = "binary surface — moves to slow E2E/smoke job (issue #1069)"]
#[test]
fn serve_acp_stdio_runs_packaged_adapter() {
    let _guard = lock_acp_cli_tests();
    let temp = TempDir::new().unwrap();
    write_fixture(&temp);
    write_file(
        temp.path(),
        "harn.toml",
        r#"
[llm]
default_provider = "openai"
"#,
    );

    let mut child = harn_e2e_command()
        .current_dir(temp.path())
        .arg("serve")
        .arg("acp")
        .arg("acp_fixture.harn")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    let (_, init) = send_request(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {}
        }),
    );
    assert_eq!(init["result"]["agentInfo"]["name"], "harn");
    assert_eq!(
        init["result"]["agentCapabilities"]["promptCapabilities"],
        json!({
            "image": true,
            "audio": true,
            "embeddedContext": false,
        })
    );

    let (_, created) = send_request(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "session/new",
            "params": {
                "cwd": temp.path(),
            }
        }),
    );
    let session_id = created["result"]["sessionId"].as_str().unwrap().to_string();

    let (notifications, response) = send_request(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "session/prompt",
            "params": {
                "sessionId": session_id,
                "prompt": [{ "type": "text", "text": "packaged" }]
            }
        }),
    );
    assert_eq!(response["result"]["stopReason"], "end_turn");
    let summary = latest_prompt_summary(&notifications, &session_id);
    assert_eq!(summary["messages"][0]["content"], "packaged");

    drop(stdin);
    let status = child.wait().unwrap();
    assert!(status.success(), "status={status}");
}

#[ignore = "binary surface — moves to slow E2E/smoke job (issue #1069)"]
#[test]
fn serve_acp_stdio_closes_sessions_with_close_and_stop_spellings() {
    let _guard = lock_acp_cli_tests();
    let temp = TempDir::new().unwrap();
    write_fixture(&temp);

    let mut child = harn_e2e_command()
        .current_dir(temp.path())
        .arg("serve")
        .arg("acp")
        .arg("acp_fixture.harn")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    for (index, method) in ["session/close", "session/stop"].into_iter().enumerate() {
        let request_base = 10 + (index as i64 * 10);
        let (_, created) = send_request(
            &mut stdin,
            &mut stdout,
            json!({
                "jsonrpc": "2.0",
                "id": request_base,
                "method": "session/new",
                "params": {
                    "cwd": temp.path(),
                }
            }),
        );
        let session_id = created["result"]["sessionId"].as_str().unwrap().to_string();

        let (_, closed) = send_request(
            &mut stdin,
            &mut stdout,
            json!({
                "jsonrpc": "2.0",
                "id": request_base + 1,
                "method": method,
                "params": {
                    "sessionId": session_id.clone(),
                }
            }),
        );
        assert_eq!(closed["result"], json!({}));

        let (_, listed) = send_request(
            &mut stdin,
            &mut stdout,
            json!({
                "jsonrpc": "2.0",
                "id": request_base + 2,
                "method": "session/list",
                "params": {}
            }),
        );
        let sessions = listed["result"]["sessions"].as_array().unwrap();
        assert!(sessions
            .iter()
            .all(|entry| entry["sessionId"].as_str() != Some(session_id.as_str())));
    }

    drop(stdin);
    let status = child.wait().unwrap();
    assert!(status.success(), "status={status}");
}
