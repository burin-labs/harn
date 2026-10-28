use super::{fs, harn_e2e_command, json, stable_request, StdioJsonRpcClient, TempDir};

pub(super) fn write_export_fixture(temp: &TempDir) {
    fs::write(
        temp.path().join("server.harn"),
        r#"
pub fn greet(name: string, excited: bool = false) -> dict {
  if excited {
    return {message: "Hello, " + name + "!"}
  }
  return {message: "Hello, " + name}
}
"#,
    )
    .unwrap();
}

#[test]
fn serve_mcp_rejects_caught_invalid_catalog_declaration() {
    let temp = TempDir::new().unwrap();
    fs::write(
        temp.path().join("server.harn"),
        r#"
fn main(harness: Harness) {
  const rejected = try {
    harness.tools.mcp_prompt(
      {name: "review", arguments: [{name: "input"}, 7], handler: {args -> args.input}},
    )
  }
  assert(is_err(rejected), "the declaration must fail")
  harness.tools.mcp_tools(tool_registry())
}
"#,
    )
    .unwrap();

    let output = harn_e2e_command()
        .current_dir(temp.path())
        .args(["serve", "mcp", "server.harn"])
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "invalid catalog must not start serving"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("invalid MCP publication") && stderr.contains("arguments[1]"),
        "startup must identify the rejected declaration: {stderr}"
    );
}

#[ignore = "binary surface — runs in the slow E2E/smoke job"]
#[test]
fn serve_mcp_dispatches_declared_tool_parameters() {
    let temp = TempDir::new().unwrap();
    fs::write(
        temp.path().join("server.harn"),
        r#"
fn main(harness: Harness) {
  tool greet(name: string, suffix: string = "!") -> string {
    return "Hello, " + name + suffix
  }
  harness.tools.mcp_tools(greet)
}
"#,
    )
    .unwrap();
    let mut command = harn_e2e_command();
    command
        .current_dir(temp.path())
        .args(["serve", "mcp", "server.harn"]);
    let mut client = StdioJsonRpcClient::spawn("declared tool MCP server", command);
    let response = client.request(stable_request(
        1,
        "tools/call",
        json!({"name": "greet", "arguments": {"name": "Ada"}}),
    ));
    assert!(response.get("error").is_none(), "{response}");
    assert_ne!(response["result"]["isError"], true, "{response}");
    assert_eq!(response["result"]["content"][0]["text"], "Hello, Ada!");
    client.shutdown_expect_success();
}
