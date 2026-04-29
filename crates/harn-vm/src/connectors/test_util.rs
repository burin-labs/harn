use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

pub(crate) use crate::triggers::test_util::clock::MockClock;

#[derive(Clone, Debug)]
pub(crate) struct CapturedHttpRequest {
    pub(crate) method: String,
    pub(crate) path: String,
    pub(crate) headers: BTreeMap<String, String>,
    pub(crate) body: String,
}

/// Cooperative accept loop: blocks until either a client connects or
/// `shutdown` flips, whichever comes first. Returns `None` on shutdown.
/// Mock servers built on top of [`spawn_mock_http_server`] use this so the
/// stub thread's lifetime is bounded by a test-owned guard rather than a
/// wall-clock deadline — the deadline approach flaked under heavy workspace
/// fan-out on CI.
fn accept_http_connection_until(
    listener: &TcpListener,
    label: &str,
    shutdown: &AtomicBool,
) -> Option<TcpStream> {
    listener
        .set_nonblocking(true)
        .expect("set listener nonblocking");
    loop {
        if shutdown.load(Ordering::Acquire) {
            return None;
        }
        match listener.accept() {
            Ok((stream, _)) => {
                stream
                    .set_nonblocking(false)
                    .expect("restore blocking mode");
                stream.set_read_timeout(Some(Duration::from_secs(30))).ok();
                stream.set_write_timeout(Some(Duration::from_secs(30))).ok();
                return Some(stream);
            }
            Err(ref error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("{label}: accept failed: {error}"),
        }
    }
}

pub(crate) fn read_http_request(stream: &mut TcpStream) -> CapturedHttpRequest {
    let mut buffer = Vec::new();
    let mut temp = [0u8; 4096];
    let header_end;
    loop {
        let n = stream.read(&mut temp).expect("read request");
        assert!(n > 0, "request ended before headers");
        buffer.extend_from_slice(&temp[..n]);
        if let Some(idx) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            header_end = idx + 4;
            break;
        }
    }

    let header_text = String::from_utf8_lossy(&buffer[..header_end]).to_string();
    let mut lines = header_text.split("\r\n").filter(|line| !line.is_empty());
    let request_line = lines.next().expect("request line");
    let mut request_parts = request_line.split_whitespace();
    let method = request_parts.next().unwrap_or_default().to_string();
    let path = request_parts.next().unwrap_or_default().to_string();
    let mut headers = BTreeMap::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.to_ascii_lowercase(), value.trim().to_string());
        }
    }

    let content_length = headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    while buffer.len() < header_end + content_length {
        let n = stream.read(&mut temp).expect("read body");
        assert!(n > 0, "request ended before body");
        buffer.extend_from_slice(&temp[..n]);
    }
    let body =
        String::from_utf8_lossy(&buffer[header_end..header_end + content_length]).to_string();

    CapturedHttpRequest {
        method,
        path,
        headers,
        body,
    }
}

/// Mock HTTP server whose lifetime is tied to a test-owned guard.
///
/// The server thread serves up to `expected_requests` requests and then
/// exits naturally. If the guard drops first (e.g. test panicked), the
/// shutdown flag flips and the server thread exits within one poll
/// iteration. There is no wall-clock deadline so the test cannot flake
/// under heavy CI fan-out, and there is no thread leak on test failure
/// because Drop signals shutdown and joins.
pub(crate) struct MockHttpServer {
    addr: SocketAddr,
    base_url: String,
    shutdown: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl MockHttpServer {
    pub(crate) fn base_url(&self) -> &str {
        &self.base_url
    }

    #[allow(dead_code)]
    pub(crate) fn addr(&self) -> SocketAddr {
        self.addr
    }
}

impl Drop for MockHttpServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Spawn a mock HTTP server bound to `127.0.0.1:0`. The supplied handler is
/// invoked per accepted request with the request index, the bound listening
/// address (so handlers can build self-referential URLs), the parsed
/// request, and the live stream so it can decide how to respond. The server
/// stops after `expected_requests` accepted requests, after the handler
/// panics, or when the returned [`MockHttpServer`] is dropped.
pub(crate) fn spawn_mock_http_server<F>(
    expected_requests: usize,
    label: &'static str,
    mut handler: F,
) -> MockHttpServer
where
    F: FnMut(usize, SocketAddr, &CapturedHttpRequest, &mut TcpStream) + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock http server");
    let addr = listener.local_addr().expect("mock http server addr");
    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_thread = shutdown.clone();
    let handle = std::thread::spawn(move || {
        for index in 0..expected_requests {
            let Some(mut stream) = accept_http_connection_until(&listener, label, &shutdown_thread)
            else {
                return;
            };
            let request = read_http_request(&mut stream);
            handler(index, addr, &request, &mut stream);
        }
    });
    MockHttpServer {
        addr,
        base_url: format!("http://{}", addr),
        shutdown,
        handle: Some(handle),
    }
}

pub(crate) fn write_http_response(
    stream: &mut TcpStream,
    status: u16,
    headers: &[(&str, String)],
    body: &str,
) {
    let status_text = match status {
        200 => "OK",
        201 => "Created",
        401 => "Unauthorized",
        429 => "Too Many Requests",
        _ => "OK",
    };
    let mut response = format!(
        "HTTP/1.1 {} {}\r\ncontent-length: {}\r\nconnection: close\r\n",
        status,
        status_text,
        body.len()
    );
    for (name, value) in headers {
        response.push_str(name);
        response.push_str(": ");
        response.push_str(value);
        response.push_str("\r\n");
    }
    response.push_str("\r\n");
    response.push_str(body);
    stream
        .write_all(response.as_bytes())
        .expect("write response");
}
