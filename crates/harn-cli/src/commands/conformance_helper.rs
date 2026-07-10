use crate::cli::{ConformanceHelperArgs, ConformanceHelperCommand, ConformanceHelperHttpProxyArgs};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

const MAX_CAPTURE_BYTES: usize = 1024 * 1024;

#[derive(Debug, PartialEq, Eq, Serialize)]
struct CapturedProxyRequest {
    method: String,
    target: String,
    headers: BTreeMap<String, String>,
    body: String,
}

pub(crate) async fn run(args: ConformanceHelperArgs) -> Result<(), String> {
    match args.command {
        ConformanceHelperCommand::HttpProxy(args) => run_http_proxy(args).await,
    }
}

async fn run_http_proxy(args: ConformanceHelperHttpProxyArgs) -> Result<(), String> {
    let state_dir = PathBuf::from(args.state_dir);
    std::fs::create_dir_all(&state_dir)
        .map_err(|error| format!("create state dir {}: {error}", state_dir.display()))?;
    let port_path = state_dir.join("port");
    let state_path = state_dir.join("state.json");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|error| format!("bind proxy listener: {error}"))?;
    let port = listener
        .local_addr()
        .map_err(|error| format!("read listener addr: {error}"))?
        .port();
    std::fs::write(&port_path, port.to_string())
        .map_err(|error| format!("write port file {}: {error}", port_path.display()))?;

    let limit = Duration::from_millis(args.timeout_ms);
    let (mut stream, _) = timeout(limit, listener.accept())
        .await
        .map_err(|_| {
            format!(
                "timed out waiting for proxy request after {}ms",
                args.timeout_ms
            )
        })?
        .map_err(|error| format!("accept proxy request: {error}"))?;
    let request = timeout(limit, read_proxy_request(&mut stream))
        .await
        .map_err(|_| {
            format!(
                "timed out reading proxy request after {}ms",
                args.timeout_ms
            )
        })??;
    let state = serde_json::to_string(&request)
        .map_err(|error| format!("serialize proxy state: {error}"))?;
    std::fs::write(&state_path, state)
        .map_err(|error| format!("write state file {}: {error}", state_path.display()))?;

    write_proxy_response(&mut stream)
        .await
        .map_err(|error| format!("write proxy response: {error}"))?;
    Ok(())
}

async fn read_proxy_request<R>(stream: &mut R) -> Result<CapturedProxyRequest, String>
where
    R: AsyncRead + Unpin,
{
    let mut buffer = Vec::new();
    let mut temp = [0_u8; 4096];
    let header_end = loop {
        let read = stream
            .read(&mut temp)
            .await
            .map_err(|error| format!("read proxy request: {error}"))?;
        if read == 0 {
            return Err("request closed before headers".to_string());
        }
        buffer.extend_from_slice(&temp[..read]);
        if buffer.len() > MAX_CAPTURE_BYTES {
            return Err(format!(
                "request exceeded {MAX_CAPTURE_BYTES} byte capture limit"
            ));
        }
        if let Some(offset) = find_double_crlf(&buffer) {
            break offset + 4;
        }
    };

    let header_text = String::from_utf8_lossy(&buffer[..header_end]);
    let mut lines = header_text.split("\r\n").filter(|line| !line.is_empty());
    let request_line = lines
        .next()
        .ok_or_else(|| "missing request line".to_string())?;
    let mut request_parts = request_line.split_whitespace();
    let method = request_parts
        .next()
        .ok_or_else(|| "missing request method".to_string())?
        .to_string();
    let target = request_parts
        .next()
        .ok_or_else(|| "missing request target".to_string())?
        .to_string();
    let _version = request_parts
        .next()
        .ok_or_else(|| "missing request version".to_string())?;

    let mut headers = BTreeMap::new();
    for line in lines {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| format!("malformed header line: {line}"))?;
        headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
    }

    let content_length = match headers.get("content-length") {
        Some(value) => value
            .parse::<usize>()
            .map_err(|error| format!("invalid content-length '{value}': {error}"))?,
        None => 0,
    };
    if content_length > MAX_CAPTURE_BYTES {
        return Err(format!(
            "body exceeded {MAX_CAPTURE_BYTES} byte capture limit"
        ));
    }
    let body_end = header_end
        .checked_add(content_length)
        .ok_or_else(|| "request body length overflow".to_string())?;
    while buffer.len() < body_end {
        let read = stream
            .read(&mut temp)
            .await
            .map_err(|error| format!("read proxy request body: {error}"))?;
        if read == 0 {
            return Err("request closed before body".to_string());
        }
        buffer.extend_from_slice(&temp[..read]);
        if buffer.len() > header_end + MAX_CAPTURE_BYTES {
            return Err(format!(
                "request exceeded {MAX_CAPTURE_BYTES} byte capture limit"
            ));
        }
    }

    Ok(CapturedProxyRequest {
        method,
        target,
        headers,
        body: String::from_utf8_lossy(&buffer[header_end..body_end]).to_string(),
    })
}

async fn write_proxy_response(stream: &mut TcpStream) -> std::io::Result<()> {
    let payload = b"proxied";
    let headers = format!(
        "HTTP/1.1 200 OK\r\ncontent-length: {}\r\ncontent-type: text/plain\r\nconnection: close\r\n\r\n",
        payload.len()
    );
    stream.write_all(headers.as_bytes()).await?;
    stream.write_all(payload).await?;
    stream.flush().await
}

fn find_double_crlf(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn raw_proxy_capture_preserves_absolute_target_and_auth() {
        let (mut client, mut server) = tokio::io::duplex(1024);
        let writer = tokio::spawn(async move {
            client
                .write_all(
                    b"GET http://example.invalid/proxy-check HTTP/1.1\r\n\
Host: example.invalid\r\n\
Proxy-Authorization: Basic dXNlcjpwYXNz\r\n\
\r\n",
                )
                .await
                .expect("write request");
        });

        let request = read_proxy_request(&mut server).await.expect("request");
        writer.await.expect("writer task");

        assert_eq!(request.method, "GET");
        assert_eq!(request.target, "http://example.invalid/proxy-check");
        assert_eq!(
            request
                .headers
                .get("proxy-authorization")
                .map(String::as_str),
            Some("Basic dXNlcjpwYXNz")
        );
        assert_eq!(request.body, "");
    }

    #[tokio::test]
    async fn raw_proxy_capture_reads_declared_body() {
        let (mut client, mut server) = tokio::io::duplex(1024);
        let writer = tokio::spawn(async move {
            client
                .write_all(
                    b"POST http://example.invalid/proxy-check HTTP/1.1\r\n\
Content-Length: 7\r\n\
\r\npayload",
                )
                .await
                .expect("write request");
        });

        let request = read_proxy_request(&mut server).await.expect("request");
        writer.await.expect("writer task");

        assert_eq!(request.method, "POST");
        assert_eq!(request.body, "payload");
    }
}
