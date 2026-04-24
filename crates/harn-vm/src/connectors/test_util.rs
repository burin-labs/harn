use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

pub(crate) use crate::triggers::test_util::clock::MockClock;

#[derive(Clone, Debug)]
pub(crate) struct CapturedHttpRequest {
    pub(crate) method: String,
    pub(crate) path: String,
    pub(crate) headers: BTreeMap<String, String>,
    pub(crate) body: String,
}

pub(crate) fn accept_http_connection(listener: &TcpListener, label: &str) -> TcpStream {
    listener
        .set_nonblocking(true)
        .expect("set listener nonblocking");
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream
                    .set_nonblocking(false)
                    .expect("restore blocking mode");
                stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
                stream.set_write_timeout(Some(Duration::from_secs(2))).ok();
                return stream;
            }
            Err(ref error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < deadline, "{label}: no client within 2s");
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
