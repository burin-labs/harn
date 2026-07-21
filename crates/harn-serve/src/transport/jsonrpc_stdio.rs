use serde_json::Value as JsonValue;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Largest `Content-Length`-framed body we will allocate for. A peer that
/// announces more than this is malfunctioning or hostile, so we reject the
/// frame at header-parse time rather than allocate an attacker-chosen
/// buffer.
///
/// `harn-dap`'s `framing` module frames the same base wire format
/// (Content-Length + CRLF) and enforces the identical 16 MiB bound as its
/// own `MAX_DAP_FRAME_BYTES`. The two are deliberately *not* shared: this
/// reader is async (`tokio::io::AsyncBufRead`) while that one is
/// synchronous `std::io::BufRead`, and no crate both depend on is a natural
/// home for stdio-transport framing. Keep the bounds in lockstep by hand.
const MAX_JSONRPC_FRAME_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum JsonRpcStdioFrameStyle {
    #[default]
    Line,
    ContentLength,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JsonRpcStdioFrame {
    pub body: Vec<u8>,
    pub style: JsonRpcStdioFrameStyle,
}

impl JsonRpcStdioFrame {
    pub fn parse_json(&self) -> Result<JsonValue, serde_json::Error> {
        serde_json::from_slice(&self.body)
    }
}

pub async fn read_jsonrpc_stdio_frame<R>(
    reader: &mut R,
) -> Result<Option<JsonRpcStdioFrame>, String>
where
    R: AsyncBufRead + Unpin,
{
    loop {
        let mut first_line = String::new();
        let bytes = reader
            .read_line(&mut first_line)
            .await
            .map_err(|error| format!("stdin read failed: {error}"))?;
        if bytes == 0 {
            return Ok(None);
        }

        let line = first_line.trim_end_matches(['\r', '\n']);
        if line.trim().is_empty() {
            continue;
        }

        if let Some(length) = content_length_from_header_line(line)? {
            return read_content_length_frame(reader, length).await.map(Some);
        }

        return Ok(Some(JsonRpcStdioFrame {
            body: line.trim().as_bytes().to_vec(),
            style: JsonRpcStdioFrameStyle::Line,
        }));
    }
}

pub async fn write_jsonrpc_stdio_message<W>(
    writer: &mut W,
    value: &JsonValue,
    style: JsonRpcStdioFrameStyle,
) -> Result<(), String>
where
    W: AsyncWrite + Unpin,
{
    let encoded = serde_json::to_vec(value).map_err(|error| format!("serialize error: {error}"))?;
    match style {
        JsonRpcStdioFrameStyle::Line => {
            writer
                .write_all(&encoded)
                .await
                .map_err(|error| format!("stdout write failed: {error}"))?;
            writer
                .write_all(b"\n")
                .await
                .map_err(|error| format!("stdout write failed: {error}"))?;
        }
        JsonRpcStdioFrameStyle::ContentLength => {
            let header = format!("Content-Length: {}\r\n\r\n", encoded.len());
            writer
                .write_all(header.as_bytes())
                .await
                .map_err(|error| format!("stdout write failed: {error}"))?;
            writer
                .write_all(&encoded)
                .await
                .map_err(|error| format!("stdout write failed: {error}"))?;
        }
    }
    writer
        .flush()
        .await
        .map_err(|error| format!("stdout flush failed: {error}"))
}

fn content_length_from_header_line(line: &str) -> Result<Option<usize>, String> {
    let Some((name, value)) = line.split_once(':') else {
        return Ok(None);
    };
    if !name.trim().eq_ignore_ascii_case("content-length") {
        return Ok(None);
    }
    let length = value
        .trim()
        .parse::<usize>()
        .map_err(|error| format!("invalid MCP Content-Length header: {error}"))?;
    if length > MAX_JSONRPC_FRAME_BYTES {
        return Err(format!(
            "MCP Content-Length {length} exceeds limit {MAX_JSONRPC_FRAME_BYTES} bytes"
        ));
    }
    Ok(Some(length))
}

async fn read_content_length_frame<R>(
    reader: &mut R,
    length: usize,
) -> Result<JsonRpcStdioFrame, String>
where
    R: AsyncBufRead + Unpin,
{
    loop {
        let mut header_line = String::new();
        let bytes = reader
            .read_line(&mut header_line)
            .await
            .map_err(|error| format!("stdin read failed: {error}"))?;
        if bytes == 0 {
            return Err("stdin closed while reading MCP Content-Length headers".to_string());
        }
        if header_line.trim_end_matches(['\r', '\n']).is_empty() {
            break;
        }
    }

    let mut body = vec![0; length];
    reader
        .read_exact(&mut body)
        .await
        .map_err(|error| format!("stdin read failed while reading MCP body: {error}"))?;
    Ok(JsonRpcStdioFrame {
        body,
        style: JsonRpcStdioFrameStyle::ContentLength,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Cursor;
    use tokio::io::BufReader;

    #[tokio::test]
    async fn reads_newline_delimited_jsonrpc_frames() {
        let mut reader = BufReader::new(Cursor::new(br#"{"jsonrpc":"2.0","id":1}"#.to_vec()));
        let frame = read_jsonrpc_stdio_frame(&mut reader)
            .await
            .expect("read")
            .expect("frame");

        assert_eq!(frame.style, JsonRpcStdioFrameStyle::Line);
        assert_eq!(frame.parse_json().expect("json")["id"], json!(1));
    }

    #[tokio::test]
    async fn reads_content_length_jsonrpc_frames() {
        let body = br#"{"jsonrpc":"2.0","id":7}"#;
        let input = format!("Content-Length: {}\r\nX-Ignored: yes\r\n\r\n", body.len());
        let mut bytes = input.into_bytes();
        bytes.extend_from_slice(body);
        let mut reader = BufReader::new(Cursor::new(bytes));
        let frame = read_jsonrpc_stdio_frame(&mut reader)
            .await
            .expect("read")
            .expect("frame");

        assert_eq!(frame.style, JsonRpcStdioFrameStyle::ContentLength);
        assert_eq!(frame.parse_json().expect("json")["id"], json!(7));
    }

    #[tokio::test]
    async fn oversized_content_length_is_rejected_without_allocating() {
        // A hostile peer announces a huge body but sends almost none. The
        // reader must reject at header-parse time rather than allocate the
        // announced buffer (previously an unbounded `vec![0; length]`).
        let declared = MAX_JSONRPC_FRAME_BYTES + 1;
        let input = format!("Content-Length: {declared}\r\n\r\nabc");
        let mut reader = BufReader::new(Cursor::new(input.into_bytes()));
        let error = read_jsonrpc_stdio_frame(&mut reader)
            .await
            .expect_err("oversized frame rejected");
        assert!(error.contains("exceeds limit"), "unexpected error: {error}");
    }

    #[tokio::test]
    async fn content_length_at_the_limit_is_accepted() {
        // Header exactly at the bound parses; a full-size body is not
        // allocated here, only the header path is exercised.
        let line = format!("Content-Length: {MAX_JSONRPC_FRAME_BYTES}");
        assert_eq!(
            content_length_from_header_line(&line).expect("at limit"),
            Some(MAX_JSONRPC_FRAME_BYTES)
        );
    }

    #[tokio::test]
    async fn writes_content_length_jsonrpc_frames() {
        let mut out = Vec::new();
        write_jsonrpc_stdio_message(
            &mut out,
            &json!({"jsonrpc":"2.0","id":9,"result":{}}),
            JsonRpcStdioFrameStyle::ContentLength,
        )
        .await
        .expect("write");

        let header_end = out
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("header end");
        let header = std::str::from_utf8(&out[..header_end]).expect("header utf8");
        let body = &out[header_end + 4..];
        assert_eq!(header, format!("Content-Length: {}", body.len()));
        assert_eq!(
            serde_json::from_slice::<JsonValue>(body).unwrap()["id"],
            json!(9)
        );
    }
}
