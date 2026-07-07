use serde_json::Value as JsonValue;
use tokio::io::{AsyncBufRead, AsyncWrite, AsyncWriteExt, Lines};

pub(super) fn split_command(command: &[String]) -> Result<(&str, &[String]), String> {
    let Some((program, argv)) = command.split_first() else {
        return Err("missing command after --".to_string());
    };
    Ok((program.as_str(), argv))
}

pub(super) fn jsonrpc_id_key(id: Option<&JsonValue>) -> Option<String> {
    id.and_then(|id| serde_json::to_string(id).ok())
}

pub(super) async fn write_json_line<W>(writer: &mut W, value: &JsonValue) -> Result<(), String>
where
    W: AsyncWrite + Unpin,
{
    let line = serde_json::to_string(value).map_err(|error| format!("encode JSON: {error}"))?;
    writer
        .write_all(line.as_bytes())
        .await
        .map_err(|error| format!("write JSON line: {error}"))?;
    writer
        .write_all(b"\n")
        .await
        .map_err(|error| format!("write JSON line: {error}"))?;
    writer
        .flush()
        .await
        .map_err(|error| format!("flush JSON line: {error}"))
}

pub(super) async fn read_until_response<R>(
    lines: &mut Lines<R>,
    target_id: &JsonValue,
    progress_token: Option<&str>,
) -> Result<(JsonValue, usize), String>
where
    R: AsyncBufRead + Unpin,
{
    let mut progress_count = 0usize;
    loop {
        let line = lines
            .next_line()
            .await
            .map_err(|error| format!("read upstream stdout: {error}"))?
            .ok_or_else(|| "upstream MCP server closed stdout".to_string())?;
        let message: JsonValue = serde_json::from_str(line.trim())
            .map_err(|error| format!("parse upstream JSON-RPC message: {error}: {line}"))?;
        if is_matching_progress(&message, progress_token) {
            progress_count += 1;
            continue;
        }
        if message.get("id") == Some(target_id) {
            return Ok((message, progress_count));
        }
    }
}

fn is_matching_progress(message: &JsonValue, progress_token: Option<&str>) -> bool {
    let Some(token) = progress_token else {
        return false;
    };
    message.get("method").and_then(JsonValue::as_str) == Some("notifications/progress")
        && message
            .get("params")
            .and_then(|params| params.get("progressToken"))
            .and_then(JsonValue::as_str)
            == Some(token)
}
