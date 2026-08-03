use serde_json::Value as JsonValue;
use tokio::io::{AsyncWrite, AsyncWriteExt};

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
