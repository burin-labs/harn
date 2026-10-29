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

fn directory_client(workspace: &Path, home: &Path) -> StdioJsonRpcClient {
    let mut command = harn_e2e_command();
    command
        .current_dir(workspace)
        .args(["serve", "acp", "--obs", "off"])
        .env_clear()
        .env("HOME", home)
        .env("PATH", std::env::var_os("PATH").unwrap_or_default());
    let mut client = StdioJsonRpcClient::spawn("ACP directory grants", command);
    let initialized = request(
        &mut client,
        json!({
            "jsonrpc":"2.0", "id":1, "method":"initialize", "params":{}
        }),
    );
    assert_eq!(
        initialized["result"]["agentCapabilities"]["sessionCapabilities"]["additionalDirectories"],
        json!({})
    );
    client
}

fn scoped_write_source(workspace: &Path, target: &Path) -> String {
    format!(
        "harness.runtime.with_execution_policy({{sandbox_profile: \"worktree\", workspace_roots: [{}], read_only_roots: [{}]}}, {{ -> harness.fs.write_text({}, \"root-grant-probe\") }})",
        json!(workspace), json!(workspace), json!(target)
    )
}

#[test]
pub(crate) fn additional_directories_grant_real_writes_and_omission_revokes_after_reload() {
    let temp = TempDir::new().unwrap();
    let base = temp.path().canonicalize().unwrap();
    let workspace = base.join("workspace");
    let first = base.join("first");
    let second = base.join("second");
    for path in [&workspace, &first, &second] {
        fs::create_dir(path).unwrap();
    }
    let directories = json!([second, first]);
    let mut client = directory_client(&workspace, &base);
    let created = request(
        &mut client,
        json!({
            "jsonrpc":"2.0", "id":2, "method":"session/new", "params":{
                "cwd":workspace, "additionalDirectories":directories,
                "environmentPolicy":{"kind":"isolated"}}
        }),
    );
    let session = created["result"]["sessionId"]
        .as_str()
        .expect("session admitted");
    let mode = request(
        &mut client,
        json!({
            "jsonrpc":"2.0", "id":20, "method":"session/set_mode",
            "params":{"sessionId":session, "modeId":"code"}
        }),
    );
    assert!(mode.get("error").is_none(), "{mode}");
    assert_eq!(
        created["result"]["session"]["additionalDirectories"], directories,
        "{created}"
    );
    for (id, directory) in [(3, &first), (4, &second)] {
        let target = directory.join("accepted");
        let response = request(
            &mut client,
            json!({
                "jsonrpc":"2.0", "id":id, "method":"session/prompt", "params":{
                    "sessionId":session, "prompt":[{"type":"text", "text":scoped_write_source(&workspace, &target)}]}
            }),
        );
        assert!(response.get("error").is_none(), "{response}");
        assert_eq!(fs::read_to_string(target).unwrap(), "root-grant-probe");
    }
    let invalid = request(
        &mut client,
        json!({
            "jsonrpc":"2.0", "id":5, "method":"session/resume", "params":{
                "sessionId":session, "additionalDirectories":[first, base.join("missing")]}
        }),
    );
    assert_eq!(invalid["error"]["code"], -32602, "{invalid}");
    assert_eq!(invalid["error"]["data"]["code"], "invalid_workspace_roots");
    client.shutdown_expect_success();

    let mut restored = directory_client(&workspace, &base);
    let listed = request(
        &mut restored,
        json!({
            "jsonrpc":"2.0", "id":2, "method":"session/list", "params":{"cwd":workspace}
        }),
    );
    let rows = listed["result"]["sessions"]
        .as_array()
        .expect("persisted rows");
    let row = rows
        .iter()
        .find(|row| row["sessionId"] == session)
        .expect("accepted session persisted");
    assert_eq!(row["additionalDirectories"], directories, "{listed}");
    let loaded = request(
        &mut restored,
        json!({
            "jsonrpc":"2.0", "id":3, "method":"session/load", "params":{
                "sessionId":session, "cwd":workspace, "environmentPolicy":{"kind":"isolated"}}
        }),
    );
    assert_eq!(
        loaded["result"]["session"]["additionalDirectories"],
        json!([]),
        "{loaded}"
    );
    let mode = request(
        &mut restored,
        json!({
            "jsonrpc":"2.0", "id":20, "method":"session/set_mode",
            "params":{"sessionId":session, "modeId":"code"}
        }),
    );
    assert!(mode.get("error").is_none(), "{mode}");
    let denied = first.join("revoked");
    let refused = request(
        &mut restored,
        json!({
            "jsonrpc":"2.0", "id":4, "method":"session/prompt", "params":{
                "sessionId":session, "prompt":[{"type":"text", "text":scoped_write_source(&workspace, &denied)}]}
        }),
    );
    assert!(refused.get("error").is_some(), "{refused}");
    assert!(
        !denied.exists(),
        "an omitted grant must not survive cold loading"
    );
    let inside = workspace.join("still-writable");
    let permitted = request(
        &mut restored,
        json!({
            "jsonrpc":"2.0", "id":5, "method":"session/prompt", "params":{
                "sessionId":session, "prompt":[{"type":"text", "text":scoped_write_source(&workspace, &inside)}]}
        }),
    );
    assert!(permitted.get("error").is_none(), "{permitted}");
    assert_eq!(fs::read_to_string(inside).unwrap(), "root-grant-probe");
    restored.shutdown_expect_success();
}

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

#[test]
pub(crate) fn additional_directories_refuse_process_escape_without_replacing_the_prior_grant() {
    let temp = TempDir::new().unwrap();
    let base = temp.path().canonicalize().unwrap();
    let workspace = base.join("workspace");
    let extra = base.join("extra");
    let outside = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    for path in [&workspace, &extra] {
        fs::create_dir(path).unwrap();
    }
    #[cfg(target_os = "macos")]
    if crate::test_util::process::unavailable_worktree_sandbox().is_some() {
        return;
    }
    let alias = base.join("escape");
    std::os::unix::fs::symlink(outside.path(), &alias).unwrap();
    let mut command = harn_e2e_command();
    command
        .current_dir(&workspace)
        .args(["serve", "acp", "--obs", "off", "--confine-workspace"])
        .arg(&base)
        .env_clear()
        .env("HOME", home.path())
        .env("PATH", std::env::var_os("PATH").unwrap_or_default());
    let mut client = StdioJsonRpcClient::spawn("ACP directory process ceiling", command);
    let initialized = request(
        &mut client,
        json!({
            "jsonrpc":"2.0", "id":1, "method":"initialize", "params":{}
        }),
    );
    assert_eq!(
        initialized["result"]["agentCapabilities"]["_meta"]["harn"]["processConfinement"]["state"],
        "confined"
    );
    let invalid = request(
        &mut client,
        json!({
            "jsonrpc":"2.0", "id":2, "method":"session/new", "params":{
                "cwd":workspace, "additionalDirectories":[extra, outside.path()],
                "environmentPolicy":{"kind":"isolated"}}
        }),
    );
    assert_eq!(invalid["error"]["code"], -32602, "{invalid}");
    assert_eq!(
        invalid["error"]["data"]["code"],
        "outside_process_confinement"
    );
    let created = request(
        &mut client,
        json!({
            "jsonrpc":"2.0", "id":3, "method":"session/new", "params":{
                "cwd":workspace, "additionalDirectories":[extra],
                "environmentPolicy":{"kind":"isolated"}}
        }),
    );
    let session = created["result"]["sessionId"]
        .as_str()
        .expect("valid root admitted");
    let mode = request(
        &mut client,
        json!({
            "jsonrpc":"2.0", "id":20, "method":"session/set_mode",
            "params":{"sessionId":session, "modeId":"code"}
        }),
    );
    assert!(mode.get("error").is_none(), "{mode}");
    let escaped = request(
        &mut client,
        json!({
            "jsonrpc":"2.0", "id":4, "method":"session/resume", "params":{
                "sessionId":session, "additionalDirectories":[extra, alias]}
        }),
    );
    assert_eq!(escaped["error"]["code"], -32602, "{escaped}");
    assert_eq!(
        escaped["error"]["data"]["code"],
        "outside_process_confinement"
    );
    let roots = request(
        &mut client,
        json!({
            "jsonrpc":"2.0", "id":5, "method":"harn.session_workspace_roots",
            "params":{"sessionId":session}
        }),
    );
    let mounted = roots["result"]["workspaceAnchor"]["additional_roots"]
        .as_array()
        .expect("known root list");
    assert_eq!(mounted.len(), 1);
    assert_eq!(mounted[0]["path"], json!(extra));
    let target = extra.join("retained-grant");
    let permitted = request(
        &mut client,
        json!({
            "jsonrpc":"2.0", "id":6, "method":"session/prompt", "params":{
                "sessionId":session, "prompt":[{"type":"text", "text":scoped_write_source(&workspace, &target)}]}
        }),
    );
    assert!(permitted.get("error").is_none(), "{permitted}");
    assert_eq!(fs::read_to_string(target).unwrap(), "root-grant-probe");
    client.shutdown_expect_success();
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

    #[cfg(target_os = "macos")]
    if let Some(refusal) = crate::test_util::process::unavailable_worktree_sandbox() {
        let output = harn_e2e_command()
            .current_dir(&workspace)
            .args(["serve", "acp", "--confine-workspace"])
            .arg(&workspace)
            .output()
            .expect("reach the actual confined ACP startup");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !output.status.success(),
            "an unsupported host cannot start a confined server"
        );
        assert!(stderr.contains(&refusal.to_string()), "{stderr}");
        assert!(
            output.stdout.is_empty(),
            "the refused server cannot serve a session"
        );
        assert!(!workspace.join("inside-probe").exists());
        assert!(!outside.join("outside-probe").exists());
        return;
    }

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

/// A module outside the confined roots is unreadable, so the importer must be
/// told the read was denied rather than that the symbol does not exist.
#[test]
pub(crate) fn confined_import_outside_the_workspace_reports_the_denied_read() {
    let temp = TempDir::new().unwrap();
    let workspace = fs::canonicalize(temp.path()).unwrap().join("workspace");
    let outside = fs::canonicalize(temp.path()).unwrap().join("outside");
    fs::create_dir_all(&workspace).unwrap();
    fs::create_dir_all(&outside).unwrap();
    #[cfg(target_os = "macos")]
    if crate::test_util::process::unavailable_worktree_sandbox().is_some() {
        return;
    }
    let library = outside.join("lib.harn");
    fs::write(&library, "pub fn greet() -> string {\n  return \"hi\"\n}\n").unwrap();
    fs::write(
        workspace.join("agent.harn"),
        format!(
            "import {{ greet }} from \"{}\"\n\npub pipeline main(harness: Harness) {{\n  \
             harness.stdio.println(greet())\n}}\n",
            outside.join("lib").display()
        ),
    )
    .unwrap();
    let mut command = harn_e2e_command();
    command
        .current_dir(&workspace)
        .args(["serve", "acp", "--confine-workspace"])
        .arg(&workspace)
        .arg("agent.harn");
    let mut client = StdioJsonRpcClient::spawn("harn serve acp", command);
    request(
        &mut client,
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
    );
    let created = request(
        &mut client,
        json!({"jsonrpc":"2.0","id":2,"method":"session/new","params":{
            "cwd": workspace, "environmentPolicy": {"kind":"isolated","grants":[]}}}),
    );
    let session_id = created["result"]["sessionId"]
        .as_str()
        .unwrap_or_else(|| panic!("session/new: {created:#}"))
        .to_string();
    let exchange = client.exchange(
        json!({"jsonrpc":"2.0","id":3,"method":"session/prompt","params":{
            "sessionId": session_id, "prompt":[{"type":"text","text":"greet"}]}}),
        |method| match method {
            "host/capabilities" => json!({}),
            "host/call" => JsonValue::Null,
            other => panic!("unexpected ACP server request: {other}"),
        },
    );
    let reported = exchange.response.to_string();
    let denied = format!(
        "read denied: {} (outside confined roots)",
        library.display()
    );
    assert!(reported.contains(&denied), "{:#}", exchange.response);
    assert!(
        !reported.contains("does not exist"),
        "{:#}",
        exchange.response
    );
    client.shutdown_expect_success();
}
