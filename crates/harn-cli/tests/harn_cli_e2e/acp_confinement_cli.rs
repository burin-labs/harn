//! `harn serve acp --confine-workspace` confines the server process itself.
//!
//! macOS confines with Seatbelt, which is process-wide. Linux confines with
//! Landlock, which covers only the calling thread and its later threads, so
//! these tests also prove the CLI confines before its runtime threads start.

use std::fs;
use std::path::Path;

use serde_json::{json, Value as JsonValue};
use tempfile::TempDir;

use crate::test_util::process::harn_e2e_command;
use crate::test_util::stdio_jsonrpc::StdioJsonRpcClient;

fn request(client: &mut StdioJsonRpcClient, request: JsonValue) -> JsonValue {
    client
        .exchange(request, |method| match method {
            "host/capabilities" => json!({}),
            // Decline host execution so Harn runs the command itself.
            "host/call" => JsonValue::Null,
            other => panic!("unexpected ACP server request: {other}"),
        })
        .response
}

/// Serve a pipeline that writes one file inside the workspace and one outside
/// it from a child shell, then report what landed. Returns the `initialize`
/// result, the command's sandbox receipt, and whether each write landed.
fn run_writes(
    workspace: &Path,
    outside: &Path,
    confine: bool,
) -> (JsonValue, JsonValue, bool, bool) {
    let inside_probe = workspace.join("inside-probe");
    let outside_probe = outside.join("outside-probe");
    fs::write(
        workspace.join("agent.harn"),
        format!(
            "pub pipeline main(harness: Harness) {{\n  \
             const r = harness.tools.run_command({{mode: \"argv\", argv: [\"sh\", \"-c\", \
             \"echo in > '{}'; echo out > '{}'\"]}})\n  \
             harness.stdio.println(json_stringify(r.sandbox))\n}}\n",
            inside_probe.display(),
            outside_probe.display()
        ),
    )
    .unwrap();
    let mut command = harn_e2e_command();
    command.current_dir(workspace).args(["serve", "acp"]);
    if confine {
        command.arg("--confine-workspace").arg(workspace);
    }
    command.arg("agent.harn");
    let mut client = StdioJsonRpcClient::spawn("harn serve acp", command);

    let init = request(
        &mut client,
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
    );
    if confine {
        let refused = request(
            &mut client,
            json!({"jsonrpc":"2.0","id":2,"method":"session/new","params":{
                "cwd": outside, "environmentPolicy": {"kind":"isolated","grants":[]}}}),
        );
        assert_eq!(
            refused["error"]["data"]["code"], "outside_process_confinement",
            "{refused:#}"
        );
    }
    let created = request(
        &mut client,
        json!({"jsonrpc":"2.0","id":3,"method":"session/new","params":{
            "cwd": workspace, "environmentPolicy": {"kind":"isolated","grants":[]}}}),
    );
    let session_id = created["result"]["sessionId"]
        .as_str()
        .unwrap_or_else(|| panic!("session/new: {created:#}"))
        .to_string();
    request(
        &mut client,
        json!({"jsonrpc":"2.0","id":4,"method":"session/set_mode",
               "params":{"sessionId": session_id, "modeId":"code"}}),
    );
    let exchange = client.exchange(
        json!({"jsonrpc":"2.0","id":5,"method":"session/prompt","params":{
            "sessionId": session_id, "prompt":[{"type":"text","text":"write"}]}}),
        |method| match method {
            "host/capabilities" => json!({}),
            "host/call" => JsonValue::Null,
            other => panic!("unexpected ACP server request: {other}"),
        },
    );
    assert_eq!(
        exchange.response["result"]["stopReason"], "end_turn",
        "{:#}",
        exchange.response
    );
    let receipt = exchange
        .notifications
        .iter()
        .find_map(|update| {
            (update["params"]["update"]["sessionUpdate"] == "agent_message_chunk")
                .then(|| update["params"]["update"]["content"]["text"].as_str())
                .flatten()
        })
        .map(|text| serde_json::from_str(text.trim()).unwrap())
        .unwrap_or_else(|| panic!("no sandbox receipt: {:#?}", exchange.notifications));
    client.shutdown_expect_success();
    (
        init["result"].clone(),
        receipt,
        inside_probe.exists(),
        outside_probe.exists(),
    )
}

#[test]
pub(crate) fn confine_workspace_holds_the_server_and_its_commands_to_the_workspace() {
    let temp = TempDir::new().unwrap();
    let workspace = fs::canonicalize(temp.path()).unwrap().join("workspace");
    let outside = fs::canonicalize(temp.path()).unwrap().join("outside");
    fs::create_dir_all(&workspace).unwrap();
    fs::create_dir_all(&outside).unwrap();

    let (init, receipt, inside, escaped) = run_writes(&workspace, &outside, true);
    let state = &init["agentCapabilities"]["_meta"]["harn"]["processConfinement"];
    assert_eq!(state["state"], "confined", "{init:#}");
    let backend = if cfg!(target_os = "linux") {
        "linux"
    } else {
        "macos"
    };
    assert_eq!(state["backend"], backend, "{init:#}");
    // The command ran under no per-turn policy, so only the process's own
    // confinement can make its receipt report enforcement.
    assert_eq!(receipt["enforced"], true, "{receipt:#}");
    assert!(inside, "a write inside the workspace must land");
    assert!(
        !escaped,
        "the kernel must refuse a write outside the workspace"
    );
}

/// Negative control: the same pipeline without the flag writes outside, so the
/// refusal above is the confinement and not the fixture.
#[test]
pub(crate) fn without_confine_workspace_the_same_write_lands_outside() {
    let temp = TempDir::new().unwrap();
    let workspace = fs::canonicalize(temp.path()).unwrap().join("workspace");
    let outside = fs::canonicalize(temp.path()).unwrap().join("outside");
    fs::create_dir_all(&workspace).unwrap();
    fs::create_dir_all(&outside).unwrap();

    let (init, receipt, inside, escaped) = run_writes(&workspace, &outside, false);
    assert_eq!(
        init["agentCapabilities"]["_meta"]["harn"]["processConfinement"]["state"],
        "unconfined"
    );
    assert_eq!(receipt["enforced"], false, "{receipt:#}");
    assert!(inside);
    assert!(escaped);
}
