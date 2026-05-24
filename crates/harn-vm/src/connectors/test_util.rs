//! Deterministic fake HTTP server for connector tests.
//!
//! `FakeHttpServer` binds `127.0.0.1:0`, drives an `accept().await` loop on
//! a tokio task, and serves each connection sequentially. The server has no
//! internal busy-wait, no spin-retry, and no wall-clock deadline — shutdown
//! is signalled via `tokio::sync::Notify` and the listener is bound to the
//! task lifetime, so dropping the server cancels the accept future without
//! leaking threads.
//!
//! Tests author one closure that turns a `FakeHttpRequest` into a
//! `FakeHttpResponse`. Captured requests are stored in-memory and accessible
//! via `requests()` / `assert_received(...)`. For pre-scripted scenarios use
//! `FakeHttpServer::scripted(label, vec![...])`.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use serde_json::Value as JsonValue;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use crate::http::framing::{http_content_length_from_headers, TEST_HTTP_MAX_BODY_BYTES};

#[allow(unused_imports)]
pub(crate) use crate::triggers::test_util::clock::MockClock;

/// HTTP request captured by [`FakeHttpServer`].
///
/// Headers are normalised to lowercase. The body is the raw bytes between
/// header end and `content-length`.
#[derive(Clone, Debug)]
pub(crate) struct FakeHttpRequest {
    pub(crate) method: String,
    pub(crate) path: String,
    pub(crate) headers: BTreeMap<String, String>,
    pub(crate) body: String,
}

impl FakeHttpRequest {
    /// Parse the body as JSON, returning `None` if the body is empty or
    /// not valid JSON.
    #[allow(dead_code)]
    pub(crate) fn body_json(&self) -> Option<JsonValue> {
        if self.body.is_empty() {
            None
        } else {
            serde_json::from_str(&self.body).ok()
        }
    }
}

/// Response written back from the fake server's handler.
#[derive(Clone, Debug)]
pub(crate) struct FakeHttpResponse {
    pub(crate) status: u16,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: Vec<u8>,
    /// Drop the connection without writing any bytes — simulates a
    /// network-level failure observed by the client.
    pub(crate) disconnect: bool,
}

impl FakeHttpResponse {
    /// 200 OK with `application/json` body.
    pub(crate) fn ok_json(body: &JsonValue) -> Self {
        Self::status_json(200, body)
    }

    /// JSON body with the given status.
    pub(crate) fn status_json(status: u16, body: &JsonValue) -> Self {
        Self {
            status,
            headers: vec![("content-type".into(), "application/json".into())],
            body: body.to_string().into_bytes(),
            disconnect: false,
        }
    }

    /// Raw text body with the given status. Defaults `content-type` to
    /// `text/plain`.
    pub(crate) fn text(status: u16, body: impl Into<String>) -> Self {
        Self {
            status,
            headers: vec![("content-type".into(), "text/plain".into())],
            body: body.into().into_bytes(),
            disconnect: false,
        }
    }

    /// Replace the entire header list.
    #[allow(dead_code)]
    pub(crate) fn with_headers<I, K, V>(mut self, headers: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        self.headers = headers
            .into_iter()
            .map(|(k, v)| (k.into(), v.into()))
            .collect();
        self
    }

    /// Override the body with raw bytes.
    #[allow(dead_code)]
    pub(crate) fn with_body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = body.into();
        self
    }

    /// Drop the connection without sending any bytes.
    #[allow(dead_code)]
    pub(crate) fn disconnect() -> Self {
        Self {
            status: 0,
            headers: Vec::new(),
            body: Vec::new(),
            disconnect: true,
        }
    }
}

/// Async fake HTTP server bound to `127.0.0.1:0`.
///
/// One closure handles every request. Captured requests are stored in
/// insertion order regardless of whether the response was written. The
/// server task accepts up to `expected_requests` connections, runs the
/// handler against each, and exits naturally — there is no wall-clock
/// deadline. Dropping the server signals shutdown and aborts the task.
pub(crate) struct FakeHttpServer {
    addr: SocketAddr,
    base_url: String,
    requests: Arc<Mutex<Vec<FakeHttpRequest>>>,
    shutdown: Arc<Notify>,
    handle: Option<JoinHandle<()>>,
}

impl FakeHttpServer {
    /// Default cap when callers don't care about the precise request count.
    const DEFAULT_CAPACITY: usize = 1024;

    /// Start a fake server with `handler` invoked per accepted request. The
    /// server accepts up to [`Self::DEFAULT_CAPACITY`] requests before
    /// exiting; use [`Self::start_with_capacity`] for a precise cap.
    #[allow(dead_code)]
    pub(crate) async fn start<F>(label: &'static str, handler: F) -> Self
    where
        F: FnMut(usize, SocketAddr, &FakeHttpRequest) -> FakeHttpResponse + Send + 'static,
    {
        Self::start_with_capacity(label, Self::DEFAULT_CAPACITY, handler).await
    }

    /// Start a fake server that accepts up to `expected_requests` and exits.
    pub(crate) async fn start_with_capacity<F>(
        label: &'static str,
        expected_requests: usize,
        mut handler: F,
    ) -> Self
    where
        F: FnMut(usize, SocketAddr, &FakeHttpRequest) -> FakeHttpResponse + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fake http server");
        let addr = listener.local_addr().expect("fake http server addr");
        let base_url = format!("http://{addr}");
        let requests: Arc<Mutex<Vec<FakeHttpRequest>>> = Arc::new(Mutex::new(Vec::new()));
        let shutdown = Arc::new(Notify::new());
        let requests_task = requests.clone();
        let shutdown_task = shutdown.clone();
        let handle = tokio::spawn(async move {
            for index in 0..expected_requests {
                let mut stream = tokio::select! {
                    _ = shutdown_task.notified() => return,
                    accept = listener.accept() => match accept {
                        Ok((stream, _)) => stream,
                        Err(error) => panic!("{label}: accept failed: {error}"),
                    },
                };
                let request = match read_request(&mut stream).await {
                    Some(request) => request,
                    None => continue,
                };
                requests_task
                    .lock()
                    .expect("fake http requests poisoned")
                    .push(request.clone());
                let response = handler(index, addr, &request);
                if let Err(error) = write_response(&mut stream, response).await {
                    // Client closed early or test panicked — record once and
                    // keep accepting so other in-flight tests aren't masked.
                    eprintln!("{label}: write failed: {error}");
                }
            }
        });
        Self {
            addr,
            base_url,
            requests,
            shutdown,
            handle: Some(handle),
        }
    }

    /// Pre-scripted responses; each accepted request consumes one entry in
    /// order. Panics if the script is exhausted.
    #[allow(dead_code)]
    pub(crate) async fn scripted(label: &'static str, script: Vec<FakeHttpResponse>) -> Self {
        let capacity = script.len();
        let mut iter = script.into_iter();
        Self::start_with_capacity(label, capacity, move |_, _, _| {
            iter.next()
                .expect("fake http: pre-scripted responses exhausted")
        })
        .await
    }

    pub(crate) fn base_url(&self) -> &str {
        &self.base_url
    }

    #[allow(dead_code)]
    pub(crate) fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Snapshot of every captured request, in receive order.
    #[allow(dead_code)]
    pub(crate) fn requests(&self) -> Vec<FakeHttpRequest> {
        self.requests
            .lock()
            .expect("fake http requests poisoned")
            .clone()
    }

    /// Find the first captured request matching `predicate`, panicking if
    /// none match. Useful for `let req = server.assert_received(|r| r.path
    /// == "/foo")` style assertions.
    #[allow(dead_code)]
    pub(crate) fn assert_received(
        &self,
        predicate: impl Fn(&FakeHttpRequest) -> bool,
    ) -> FakeHttpRequest {
        let snapshot = self.requests();
        snapshot
            .into_iter()
            .find(predicate)
            .expect("fake http: no captured request matched the predicate")
    }
}

impl Drop for FakeHttpServer {
    fn drop(&mut self) {
        self.shutdown.notify_waiters();
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}

async fn read_request(stream: &mut TcpStream) -> Option<FakeHttpRequest> {
    let mut buffer: Vec<u8> = Vec::new();
    let mut temp = [0u8; 4096];
    let header_end = loop {
        let n = match stream.read(&mut temp).await {
            Ok(n) => n,
            Err(_) => return None,
        };
        if n == 0 {
            return None;
        }
        buffer.extend_from_slice(&temp[..n]);
        if let Some(idx) = find_double_crlf(&buffer) {
            break idx + 4;
        }
    };

    let header_text = String::from_utf8_lossy(&buffer[..header_end]).to_string();
    let mut lines = header_text.split("\r\n").filter(|line| !line.is_empty());
    let request_line = lines.next()?;
    let mut request_parts = request_line.split_whitespace();
    let method = request_parts.next()?.to_string();
    let path = request_parts.next()?.to_string();
    let mut headers = BTreeMap::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.to_ascii_lowercase(), value.trim().to_string());
        }
    }

    let content_length = match http_content_length_from_headers(&headers, TEST_HTTP_MAX_BODY_BYTES)
    {
        Ok(content_length) => content_length,
        Err(_) => return None,
    };
    let body_end = header_end.checked_add(content_length)?;
    while buffer.len() < body_end {
        let n = match stream.read(&mut temp).await {
            Ok(n) => n,
            Err(_) => return None,
        };
        if n == 0 {
            return None;
        }
        buffer.extend_from_slice(&temp[..n]);
    }
    let body = String::from_utf8_lossy(&buffer[header_end..body_end]).to_string();

    Some(FakeHttpRequest {
        method,
        path,
        headers,
        body,
    })
}

fn find_double_crlf(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

async fn write_response(stream: &mut TcpStream, response: FakeHttpResponse) -> std::io::Result<()> {
    if response.disconnect {
        return Ok(());
    }
    let status_text = status_reason(response.status);
    let mut header_block = format!(
        "HTTP/1.1 {} {}\r\ncontent-length: {}\r\nconnection: close\r\n",
        response.status,
        status_text,
        response.body.len(),
    );
    for (name, value) in &response.headers {
        header_block.push_str(name);
        header_block.push_str(": ");
        header_block.push_str(value);
        header_block.push_str("\r\n");
    }
    header_block.push_str("\r\n");
    stream.write_all(header_block.as_bytes()).await?;
    stream.write_all(&response.body).await?;
    stream.flush().await?;
    Ok(())
}

fn status_reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        204 => "No Content",
        301 => "Moved Permanently",
        302 => "Found",
        304 => "Not Modified",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        422 => "Unprocessable Entity",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "OK",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    async fn http_get(url: &str) -> (u16, String) {
        let response = reqwest::get(url).await.expect("get");
        let status = response.status().as_u16();
        let body = response.text().await.expect("body");
        (status, body)
    }

    #[tokio::test]
    async fn handler_serves_one_request_and_captures_it() {
        let server =
            FakeHttpServer::start_with_capacity("fake-test", 1, |_index, _addr, request| {
                assert_eq!(request.method, "GET");
                FakeHttpResponse::ok_json(&json!({"ok": true, "path": request.path}))
            })
            .await;
        let url = format!("{}/probe", server.base_url());
        let (status, body) = http_get(&url).await;
        assert_eq!(status, 200);
        assert!(body.contains("\"path\":\"/probe\""));
        assert_eq!(server.requests().len(), 1);
        let captured = server.assert_received(|req| req.path == "/probe");
        assert_eq!(captured.method, "GET");
    }

    #[tokio::test]
    async fn scripted_responses_replay_in_order() {
        let server = FakeHttpServer::scripted(
            "scripted",
            vec![
                FakeHttpResponse::ok_json(&json!({"index": 0})),
                FakeHttpResponse::status_json(429, &json!({"index": 1})),
            ],
        )
        .await;
        let url = format!("{}/", server.base_url());
        let (status_a, body_a) = http_get(&url).await;
        let (status_b, body_b) = http_get(&url).await;
        assert_eq!(status_a, 200);
        assert!(body_a.contains("\"index\":0"));
        assert_eq!(status_b, 429);
        assert!(body_b.contains("\"index\":1"));
    }

    #[tokio::test]
    async fn oversized_content_length_is_dropped_without_capture() {
        let server =
            FakeHttpServer::start_with_capacity("oversized-body", 1, |_index, _addr, _request| {
                FakeHttpResponse::text(200, "unexpected")
            })
            .await;
        let mut stream = TcpStream::connect(server.addr()).await.expect("connect");
        let request = format!(
            "POST /oversized HTTP/1.1\r\ncontent-length: {}\r\n\r\n",
            TEST_HTTP_MAX_BODY_BYTES + 1
        );
        stream.write_all(request.as_bytes()).await.expect("write");
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.expect("read eof");

        assert!(response.is_empty());
        assert!(server.requests().is_empty());
    }

    #[tokio::test]
    async fn body_json_decodes_request_payload() {
        let server = FakeHttpServer::start_with_capacity("body-json", 1, |_, _, request| {
            let payload = request.body_json().expect("json body");
            assert_eq!(payload["query"], json!("ping"));
            FakeHttpResponse::ok_json(&json!({"echo": payload}))
        })
        .await;
        let url = format!("{}/echo", server.base_url());
        let response = reqwest::Client::new()
            .post(&url)
            .json(&json!({"query": "ping"}))
            .send()
            .await
            .expect("post");
        assert_eq!(response.status().as_u16(), 200);
        let body = response.text().await.expect("text");
        assert!(body.contains("\"echo\":{\"query\":\"ping\"}"));
    }
}
