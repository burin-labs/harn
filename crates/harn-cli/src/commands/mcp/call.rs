use std::process::Stdio;

use harn_vm::mcp_protocol::PROTOCOL_VERSION as MCP_PROTOCOL_VERSION;
use serde::Serialize;
use serde_json::{json, Value as JsonValue};
use tokio::io::{AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader, Lines};
use tokio::process::Command;

use crate::cli::McpCallArgs;

use super::stdio_client::{read_until_response, split_command, write_json_line};

#[derive(Serialize)]
struct McpCallReport {
    result: JsonValue,
    progress_count: usize,
}

pub(crate) async fn run(args: &McpCallArgs) -> Result<i32, String> {
    let arguments: JsonValue = serde_json::from_str(&args.arguments)
        .map_err(|error| format!("parse --arguments JSON: {error}"))?;
    if !arguments.is_object() {
        return Err("--arguments must be a JSON object".to_string());
    }

    let (program, argv) = split_command(&args.command)?;
    let mut child = Command::new(program)
        .args(argv)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("spawn stdio MCP server `{program}`: {error}"))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "stdio MCP server stdin was not piped".to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "stdio MCP server stdout was not piped".to_string())?;
    let mut lines = BufReader::new(stdout).lines();

    let report = call_tool(args, arguments, &mut stdin, &mut lines).await;
    let _ = stdin.shutdown().await;
    drop(stdin);
    let cleanup = finish_one_shot_server(&mut child).await;
    let report = report?;
    cleanup?;
    println!(
        "{}",
        serde_json::to_string_pretty(&report)
            .map_err(|error| format!("serialize MCP call report: {error}"))?
    );
    Ok(0)
}

async fn finish_one_shot_server(child: &mut tokio::process::Child) -> Result<(), String> {
    if let Some(status) = child
        .try_wait()
        .map_err(|error| format!("poll stdio MCP server exit: {error}"))?
    {
        return if status.success() {
            Ok(())
        } else {
            Err(format!("stdio MCP server exited with {status}"))
        };
    }
    child
        .start_kill()
        .map_err(|error| format!("terminate stdio MCP server after one-shot call: {error}"))?;
    let _ = child
        .wait()
        .await
        .map_err(|error| format!("wait for terminated stdio MCP server: {error}"))?;
    Ok(())
}

async fn call_tool<W, R>(
    args: &McpCallArgs,
    arguments: JsonValue,
    stdin: &mut W,
    lines: &mut Lines<R>,
) -> Result<McpCallReport, String>
where
    W: AsyncWrite + Unpin,
    R: tokio::io::AsyncBufRead + Unpin,
{
    write_json_line(
        stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "harn-mcp-call", "version": env!("CARGO_PKG_VERSION")},
            },
        }),
    )
    .await?;
    let (initialize, _) = read_until_response(lines, &JsonValue::from(1), None).await?;
    if let Some(error) = initialize.get("error") {
        return Err(format!("initialize failed: {error}"));
    }

    let mut params = json!({
        "name": args.tool,
        "arguments": arguments,
    });
    if let Some(token) = args.progress_token.as_ref() {
        params["_meta"] = json!({"progressToken": token});
    }
    write_json_line(
        stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": params,
        }),
    )
    .await?;
    let (response, progress_count) =
        read_until_response(lines, &JsonValue::from(2), args.progress_token.as_deref()).await?;
    if let Some(error) = response.get("error") {
        return Err(format!("tools/call failed: {error}"));
    }
    let result = response
        .get("result")
        .ok_or_else(|| format!("tools/call response missing result: {response}"))?;
    if result.get("isError").and_then(JsonValue::as_bool) == Some(true) {
        return Err(tool_error_message(result));
    }
    Ok(McpCallReport {
        result: result
            .get("structuredContent")
            .cloned()
            .unwrap_or_else(|| result.clone()),
        progress_count,
    })
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
