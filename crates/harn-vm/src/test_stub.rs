//! Shared building blocks for in-process TCP stubs used by VM tests.
//!
//! The pattern here replaces wall-clock-deadline accept loops (which flake
//! under heavy nextest fan-out on CI) with a cooperative shutdown flag owned
//! by an RAII guard. The guard joins the stub thread on Drop, so the stub's
//! lifetime is bounded by the test's local scope rather than by a deadline,
//! and threads can never leak past the leak-timeout window even on panic.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

/// Cooperative blocking accept: returns the next client connection, or
/// `None` once `shutdown` flips. Sets the listener nonblocking and polls
/// every 5ms. The returned stream is restored to blocking mode and given
/// 30-second read/write timeouts so a misbehaving handler can't wedge the
/// stub thread forever.
pub(crate) fn accept_until(
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

/// RAII guard for an in-process stub. Dropping flips the shutdown flag and
/// joins the worker thread.
pub(crate) struct StubServer {
    addr: SocketAddr,
    shutdown: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl StubServer {
    pub(crate) fn addr(&self) -> SocketAddr {
        self.addr
    }

    #[allow(dead_code)]
    pub(crate) fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }
}

impl Drop for StubServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Bind a localhost listener and run `body` on a worker thread once a
/// client connects. The closure receives the accepted [`TcpStream`] by
/// value so it can wrap it in TLS or otherwise consume it. The returned
/// [`StubServer`] guard binds the worker's lifetime to the test scope.
pub(crate) fn spawn_stub<F>(label: &'static str, body: F) -> StubServer
where
    F: FnOnce(TcpStream) + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub listener");
    let addr = listener.local_addr().expect("stub addr");
    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_thread = shutdown.clone();
    let handle = std::thread::spawn(move || {
        let Some(stream) = accept_until(&listener, label, &shutdown_thread) else {
            return;
        };
        body(stream);
    });
    StubServer {
        addr,
        shutdown,
        handle: Some(handle),
    }
}

/// Like [`spawn_stub`] but for stubs that must accept multiple connections
/// in sequence. The handler is invoked per accepted stream with the request
/// index. The worker exits naturally after `expected` accepts or when the
/// guard is dropped, whichever comes first.
pub(crate) fn spawn_stub_n<F>(expected: usize, label: &'static str, mut body: F) -> StubServer
where
    F: FnMut(usize, TcpStream) + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub listener");
    let addr = listener.local_addr().expect("stub addr");
    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_thread = shutdown.clone();
    let handle = std::thread::spawn(move || {
        for index in 0..expected {
            let Some(stream) = accept_until(&listener, label, &shutdown_thread) else {
                return;
            };
            body(index, stream);
        }
    });
    StubServer {
        addr,
        shutdown,
        handle: Some(handle),
    }
}

/// Read an HTTP/1.1 request from `stream`, returning the raw byte buffer
/// (headers + body). Reads until a `\r\n\r\n` separator is found, then
/// continues reading until the body matches `Content-Length` (if present).
/// Useful for stubs that want to inspect or capture the full request bytes
/// without parsing structure.
pub(crate) fn read_http_request_bytes(stream: &mut TcpStream) -> Vec<u8> {
    let mut data = Vec::new();
    let mut buf = [0u8; 4096];
    let header_end = loop {
        let n = stream.read(&mut buf).expect("read request");
        assert!(n > 0, "request ended before headers");
        data.extend_from_slice(&buf[..n]);
        if let Some(idx) = data.windows(4).position(|window| window == b"\r\n\r\n") {
            break idx + 4;
        }
    };
    let header_text = String::from_utf8_lossy(&data[..header_end]);
    let content_length = header_text
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    while data.len() < header_end + content_length {
        let n = stream.read(&mut buf).expect("read request body");
        assert!(n > 0, "request ended before body");
        data.extend_from_slice(&buf[..n]);
    }
    data
}

/// Write a complete HTTP/1.1 response. `extra_headers` are inserted between
/// `content-length` and the blank line. Status text is filled in for common
/// codes; unknown codes default to "OK" (callers should pass strings via
/// the response body or rely on test clients that only inspect status).
pub(crate) fn write_http_response(
    stream: &mut TcpStream,
    status: u16,
    extra_headers: &[(&str, &str)],
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
        "HTTP/1.1 {} {}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n",
        status,
        status_text,
        body.len()
    );
    for (name, value) in extra_headers {
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
