use serde::Serialize;
use serde_json::{json, Value as JsonValue};

use crate::cli::McpCallArgs;

use super::stdio_client::split_command;

#[derive(Serialize)]
struct McpCallReport {
    result: JsonValue,
}

pub(crate) async fn run(args: &McpCallArgs) -> Result<i32, String> {
    let arguments: JsonValue = serde_json::from_str(&args.arguments)
        .map_err(|error| format!("parse --arguments JSON: {error}"))?;
    if !arguments.is_object() {
        return Err("--arguments must be a JSON object".to_string());
    }

    let (program, argv) = split_command(&args.command)?;
    let spec = json!({
        "name": "harn-mcp-call",
        "transport": "stdio",
        "command": program,
        "args": argv,
    });
    let client = harn_vm::mcp::connect_mcp_server_from_json(&spec)
        .await
        .map_err(|error| format!("connect MCP server: {error}"))?;

    let mut params = json!({
        "name": args.tool,
        "arguments": arguments,
    });
    if let Some(token) = args.progress_token.as_ref() {
        params["_meta"] = json!({"progressToken": token});
    }
    let result = client
        .call("tools/call", params)
        .await
        .map_err(|error| format!("tools/call failed: {error}"))?;
    if result.get("isError").and_then(JsonValue::as_bool) == Some(true) {
        return Err(tool_error_message(&result));
    }

    let report = McpCallReport {
        result: result.get("structuredContent").cloned().unwrap_or(result),
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&report)
            .map_err(|error| format!("serialize MCP call report: {error}"))?
    );
    Ok(0)
}

fn tool_error_message(result: &JsonValue) -> String {
    result
        .get("content")
        .and_then(JsonValue::as_array)
        .and_then(|content| content.first())
        .and_then(|item| item.get("text"))
        .and_then(JsonValue::as_str)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format!("MCP tool returned isError: {result}"))
}
