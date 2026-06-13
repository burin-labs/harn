use super::client::{
    compute_retry_delay, parse_retry_after_value, vm_execute_http_request, vm_http_download,
    vm_http_stream_info, vm_http_stream_open, vm_http_stream_read,
};
use super::streaming::{
    vm_sse_event_frame, vm_sse_server_cancel, vm_sse_server_heartbeat,
    vm_sse_server_mock_disconnect, vm_sse_server_mock_receive, vm_sse_server_observed_bool,
    vm_sse_server_response, vm_sse_server_send,
};
use super::{
    handle_from_value, http_mock_calls_snapshot, mock_call_headers_value, push_http_mock,
    redact_mock_call_url, reset_http_state, HttpMockResponse,
};
use crate::connectors::test_util::{FakeHttpResponse, FakeHttpServer};
use crate::value::VmValue;
use base64::Engine;
use rcgen::generate_simple_self_signed;
use rustls::pki_types::PrivatePkcs8KeyDer;
use rustls::{ServerConfig, ServerConnection, StreamOwned};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Once};
use std::time::{Duration, UNIX_EPOCH};
use tempfile::TempDir;
use x509_parser::prelude::{FromDer, X509Certificate};

fn expect_bool(value: VmValue) -> bool {
    let VmValue::Bool(value) = value else {
        panic!("expected bool, got {}", value.display());
    };
    value
}

#[test]
fn parses_retry_after_delta_seconds() {
    assert_eq!(parse_retry_after_value("5"), Some(Duration::from_secs(5)));
}

#[test]
fn parses_retry_after_http_date() {
    let now = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let header = httpdate::fmt_http_date(now + Duration::from_secs(2));
    let parsed =
        super::client::parse_retry_after_value_at(&header, now).expect("http-date should parse");
    assert_eq!(parsed, Duration::from_secs(2));
}

#[test]
fn malformed_retry_after_returns_none() {
    assert_eq!(parse_retry_after_value("soon-ish"), None);
}

#[test]
fn retry_delay_honors_retry_after_floor() {
    let delay = compute_retry_delay(0, 1, Some(Duration::from_millis(250)));
    assert!(delay >= Duration::from_millis(250));
    assert!(delay <= Duration::from_mins(1));
}

#[tokio::test]
async fn typed_mock_api_drives_http_request_retries() {
    reset_http_state();
    push_http_mock(
        "GET",
        "https://api.example.com/retry",
        vec![
            HttpMockResponse::new(503, "busy").with_header("retry-after", "0"),
            HttpMockResponse::new(200, "ok"),
        ],
    );
    let result = vm_execute_http_request(
        "GET",
        "https://api.example.com/retry",
        &crate::value::DictMap::from_iter([
            ("retries".to_string(), VmValue::Int(1)),
            ("backoff".to_string(), VmValue::Int(0)),
        ]),
    )
    .await
    .expect("mocked request should succeed after retry");

    let dict = result.as_dict().expect("response dict");
    assert_eq!(dict["status"].as_int(), Some(200));
    let calls = http_mock_calls_snapshot();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].url, "https://api.example.com/retry");
    reset_http_state();
}

#[tokio::test]
async fn http_mock_records_normalized_headers_and_final_query_url() {
    reset_http_state();
    push_http_mock(
        "GET",
        "https://api.example.com/items?api_key=secret&limit=2",
        vec![HttpMockResponse::new(200, "ok")],
    );
    let options = crate::value::DictMap::from_iter([
        (
            "headers".to_string(),
            VmValue::dict(crate::value::DictMap::from_iter([
                (
                    "Authorization".to_string(),
                    VmValue::String(std::sync::Arc::from("Bearer secret")),
                ),
                (
                    "X-Trace".to_string(),
                    VmValue::String(std::sync::Arc::from("trace-1")),
                ),
            ])),
        ),
        (
            "query".to_string(),
            VmValue::dict(crate::value::DictMap::from_iter([
                (
                    "api_key".to_string(),
                    VmValue::String(std::sync::Arc::from("secret")),
                ),
                ("limit".to_string(), VmValue::Int(2)),
            ])),
        ),
    ]);

    let response = vm_execute_http_request("GET", "https://api.example.com/items", &options)
        .await
        .expect("mocked request with query");
    let response = response.as_dict().expect("response dict");
    assert_eq!(response["status"].as_int(), Some(200));

    let calls = http_mock_calls_snapshot();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].url,
        "https://api.example.com/items?api_key=secret&limit=2"
    );
    assert_eq!(
        calls[0].headers.get("authorization").map(String::as_str),
        Some("Bearer secret")
    );
    assert_eq!(
        calls[0].headers.get("x-trace").map(String::as_str),
        Some("trace-1")
    );
    reset_http_state();
}

#[test]
fn mock_call_headers_redact_sensitive_values() {
    let headers = crate::value::DictMap::from_iter([
        (
            "authorization".to_string(),
            VmValue::String(std::sync::Arc::from("Bearer secret")),
        ),
        (
            "accept".to_string(),
            VmValue::String(std::sync::Arc::from("application/json")),
        ),
        (
            "x-api-key".to_string(),
            VmValue::String(std::sync::Arc::from("secret")),
        ),
    ]);
    let redacted = mock_call_headers_value(&headers, true);
    assert_eq!(redacted["authorization"].display(), "[redacted]");
    assert_eq!(redacted["x-api-key"].display(), "[redacted]");
    assert_eq!(redacted["accept"].display(), "application/json");

    let raw = mock_call_headers_value(&headers, false);
    assert_eq!(raw["authorization"].display(), "Bearer secret");
}

#[test]
fn mock_call_url_redacts_sensitive_query_values() {
    assert_eq!(
        redact_mock_call_url(
            "https://api.example.com/items?api_key=secret&limit=2&access_token=token",
            true,
        ),
        "https://api.example.com/items?api_key=%5Bredacted%5D&limit=2&access_token=%5Bredacted%5D"
    );
    assert_eq!(
        redact_mock_call_url("https://api.example.com/items?api_key=secret", false),
        "https://api.example.com/items?api_key=secret"
    );
    assert_eq!(
        redact_mock_call_url("https://api.example.com/items?q=a%20b", true),
        "https://api.example.com/items?q=a%20b"
    );
}

#[tokio::test]
async fn multipart_requests_are_mock_visible() {
    reset_http_state();
    push_http_mock(
        "POST",
        "https://api.example.com/upload",
        vec![HttpMockResponse::new(201, "uploaded")],
    );
    let options = crate::value::DictMap::from_iter([(
        "multipart".to_string(),
        VmValue::List(std::sync::Arc::new(vec![
            VmValue::dict(crate::value::DictMap::from_iter([
                (
                    "name".to_string(),
                    VmValue::String(std::sync::Arc::from("meta")),
                ),
                (
                    "value".to_string(),
                    VmValue::String(std::sync::Arc::from("hello")),
                ),
            ])),
            VmValue::dict(crate::value::DictMap::from_iter([
                (
                    "name".to_string(),
                    VmValue::String(std::sync::Arc::from("blob")),
                ),
                (
                    "filename".to_string(),
                    VmValue::String(std::sync::Arc::from("blob.bin")),
                ),
                (
                    "content_type".to_string(),
                    VmValue::String(std::sync::Arc::from("application/octet-stream")),
                ),
                (
                    "value".to_string(),
                    VmValue::Bytes(std::sync::Arc::new(vec![0, 1, 2, 3])),
                ),
            ])),
        ])),
    )]);

    let response = vm_execute_http_request("POST", "https://api.example.com/upload", &options)
        .await
        .expect("multipart mock request should succeed");
    let response = response.as_dict().expect("response dict");
    assert_eq!(response["status"].as_int(), Some(201));

    let calls = http_mock_calls_snapshot();
    assert_eq!(calls.len(), 1);
    assert!(calls[0]
        .headers
        .get("content-type")
        .expect("content-type recorded")
        .contains("multipart/form-data"));
    let body = calls[0].body.as_deref().expect("multipart body recorded");
    assert!(body.contains("name=\"meta\""));
    assert!(body.contains("hello"));
    assert!(body.contains("filename=\"blob.bin\""));
    reset_http_state();
}

#[tokio::test]
async fn http_stream_mock_reads_in_chunks() {
    reset_http_state();
    push_http_mock(
        "GET",
        "https://api.example.com/stream",
        vec![HttpMockResponse::new(200, "stream-body")],
    );

    let handle = vm_http_stream_open(
        "https://api.example.com/stream",
        &crate::value::DictMap::new(),
    )
    .await
    .expect("stream open");
    let stream_id = handle.display();
    let info = vm_http_stream_info(&stream_id).expect("stream info");
    let info = info.as_dict().expect("info dict");
    assert_eq!(info["status"].as_int(), Some(200));

    let first = vm_http_stream_read(&stream_id, 6)
        .await
        .expect("first chunk");
    let second = vm_http_stream_read(&stream_id, 64)
        .await
        .expect("second chunk");
    let end = vm_http_stream_read(&stream_id, 64)
        .await
        .expect("end marker");
    assert_eq!(first.as_bytes().expect("bytes"), b"stream");
    assert_eq!(second.as_bytes().expect("bytes"), b"-body");
    assert!(matches!(end, VmValue::Nil));
    reset_http_state();
}

#[tokio::test]
async fn http_download_mock_writes_file() {
    reset_http_state();
    let temp = TempDir::new().expect("tempdir");
    let path = temp.path().join("download.bin");
    push_http_mock(
        "GET",
        "https://api.example.com/download",
        vec![HttpMockResponse::new(200, "downloaded")],
    );

    let response = vm_http_download(
        "https://api.example.com/download",
        &path.display().to_string(),
        &crate::value::DictMap::new(),
    )
    .await
    .expect("download response");
    let response = response.as_dict().expect("response dict");
    assert_eq!(response["bytes_written"].as_int(), Some(10));
    assert_eq!(
        std::fs::read_to_string(path).expect("downloaded file"),
        "downloaded"
    );
    reset_http_state();
}

#[tokio::test]
async fn http_download_mock_retries_retryable_status() {
    reset_http_state();
    let temp = TempDir::new().expect("tempdir");
    let path = temp.path().join("download.bin");
    push_http_mock(
        "GET",
        "https://api.example.com/download-retry",
        vec![
            HttpMockResponse::new(503, "busy").with_header("retry-after", "0"),
            HttpMockResponse::new(200, "downloaded"),
        ],
    );

    let response = vm_http_download(
        "https://api.example.com/download-retry",
        &path.display().to_string(),
        &crate::value::DictMap::from_iter([(
            "retry".to_string(),
            VmValue::dict(crate::value::DictMap::from_iter([
                ("max".to_string(), VmValue::Int(1)),
                ("backoff_ms".to_string(), VmValue::Int(0)),
            ])),
        )]),
    )
    .await
    .expect("download response after retry");

    let response = response.as_dict().expect("response dict");
    assert_eq!(response["status"].as_int(), Some(200));
    assert_eq!(
        std::fs::read_to_string(path).expect("downloaded file"),
        "downloaded"
    );
    assert_eq!(http_mock_calls_snapshot().len(), 2);
    reset_http_state();
}

#[tokio::test]
async fn http_download_mock_enforces_max_response_bytes() {
    reset_http_state();
    let temp = TempDir::new().expect("tempdir");
    let path = temp.path().join("download.bin");
    push_http_mock(
        "GET",
        "https://api.example.com/download-too-large",
        vec![HttpMockResponse::new(200, "too-large")],
    );

    let error = vm_http_download(
        "https://api.example.com/download-too-large",
        &path.display().to_string(),
        &crate::value::DictMap::from_iter([("max_response_bytes".to_string(), VmValue::Int(3))]),
    )
    .await
    .expect_err("oversized mock body should fail");

    assert!(error
        .to_string()
        .contains("response body exceeded max_response_bytes"));
    assert!(
        !path.exists(),
        "oversized mock response must be rejected before creating the destination"
    );
    reset_http_state();
}

#[tokio::test]
async fn http_download_oversize_stream_preserves_existing_file() {
    reset_http_state();
    let temp = TempDir::new().expect("tempdir");
    let path = temp.path().join("download.bin");
    std::fs::write(&path, "original").expect("seed existing file");

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
    let port = listener.local_addr().expect("listener addr").port();
    let thread = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept client");
        let request = read_http_request_generic(&mut stream);
        assert!(request.starts_with("GET /oversize HTTP/1.1\r\n"));
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\nconnection: close\r\n\r\nab",
            )
            .expect("write response");
        stream.flush().expect("flush response");
    });

    let error = vm_http_download(
        &format!("http://127.0.0.1:{port}/oversize"),
        &path.display().to_string(),
        &crate::value::DictMap::from_iter([("max_response_bytes".to_string(), VmValue::Int(1))]),
    )
    .await
    .expect_err("oversized stream should fail");

    assert!(error
        .to_string()
        .contains("response body exceeded max_response_bytes"));
    assert_eq!(
        std::fs::read_to_string(&path).expect("existing file"),
        "original"
    );
    thread.join().expect("server thread");
    reset_http_state();
}

#[tokio::test]
async fn http_proxy_routes_requests_through_configured_proxy() {
    reset_http_state();
    let proxy =
        FakeHttpServer::start_with_capacity("proxy listener", 1, |_index, _addr, request| {
            assert_eq!(request.method, "GET");
            assert_eq!(request.path, "http://example.invalid/proxy-check");
            assert_eq!(
                request
                    .headers
                    .get("proxy-authorization")
                    .map(String::as_str),
                Some("Basic dXNlcjpwYXNz")
            );
            FakeHttpResponse::text(200, "proxied")
        })
        .await;

    let options = crate::value::DictMap::from_iter([
        (
            "proxy".to_string(),
            VmValue::String(std::sync::Arc::from(proxy.base_url().to_string())),
        ),
        (
            "proxy_auth".to_string(),
            VmValue::dict(crate::value::DictMap::from_iter([
                (
                    "user".to_string(),
                    VmValue::String(std::sync::Arc::from("user")),
                ),
                (
                    "pass".to_string(),
                    VmValue::String(std::sync::Arc::from("pass")),
                ),
            ])),
        ),
        ("timeout_ms".to_string(), VmValue::Int(1_000)),
    ]);

    let response = vm_execute_http_request("GET", "http://example.invalid/proxy-check", &options)
        .await
        .expect("proxied response");
    let response = response.as_dict().expect("response dict");
    assert_eq!(response["status"].as_int(), Some(200));
    assert_eq!(response["body"].display(), "proxied");
    drop(proxy);
    reset_http_state();
}

#[tokio::test]
async fn custom_tls_ca_bundle_and_pin_allow_request() {
    reset_http_state();
    install_rustls_provider();
    let temp = TempDir::new().expect("tempdir");
    let cert = generate_simple_self_signed(vec!["localhost".to_string(), "127.0.0.1".to_string()])
        .expect("generate cert");
    let cert_pem = cert.cert.pem();
    let cert_path = temp.path().join("cert.pem");
    std::fs::write(&cert_path, cert_pem.as_bytes()).expect("write cert");
    let pin = spki_pin_base64(cert.cert.der().as_ref());

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind tls listener");
    let port = listener.local_addr().expect("tls addr").port();
    let server_config = Arc::new(
        ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![cert.cert.der().clone()],
                PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der()).into(),
            )
            .expect("build tls config"),
    );
    let thread = std::thread::spawn(move || {
        let (tcp, _) = listener.accept().expect("accept tls client");
        let conn = ServerConnection::new(server_config).expect("server connection");
        let mut stream = StreamOwned::new(conn, tcp);
        let request = read_http_request_generic(&mut stream);
        assert!(request.starts_with("GET /secure HTTP/1.1\r\n"));
        write_http_response_generic(
            &mut stream,
            200,
            &[("content-type", "text/plain".to_string())],
            "secure",
        );
    });

    let options = crate::value::DictMap::from_iter([(
        "tls".to_string(),
        VmValue::dict(crate::value::DictMap::from_iter([
            (
                "ca_bundle_path".to_string(),
                VmValue::String(std::sync::Arc::from(cert_path.display().to_string())),
            ),
            (
                "pinned_sha256".to_string(),
                VmValue::List(std::sync::Arc::new(vec![VmValue::String(
                    std::sync::Arc::from(pin),
                )])),
            ),
        ])),
    )]);
    let response =
        vm_execute_http_request("GET", &format!("https://localhost:{port}/secure"), &options)
            .await
            .expect("tls request should succeed");
    let response = response.as_dict().expect("response dict");
    assert_eq!(response["body"].display(), "secure");
    thread.join().expect("tls thread");
    reset_http_state();
}

#[tokio::test]
async fn custom_tls_pin_mismatch_is_rejected() {
    reset_http_state();
    install_rustls_provider();
    let temp = TempDir::new().expect("tempdir");
    let cert = generate_simple_self_signed(vec!["localhost".to_string(), "127.0.0.1".to_string()])
        .expect("generate cert");
    let cert_pem = cert.cert.pem();
    let cert_path = temp.path().join("cert.pem");
    std::fs::write(&cert_path, cert_pem.as_bytes()).expect("write cert");

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind tls listener");
    let port = listener.local_addr().expect("tls addr").port();
    let server_config = Arc::new(
        ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![cert.cert.der().clone()],
                PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der()).into(),
            )
            .expect("build tls config"),
    );
    let thread = std::thread::spawn(move || {
        let (tcp, _) = listener.accept().expect("accept tls client");
        let conn = ServerConnection::new(server_config).expect("server connection");
        let mut stream = StreamOwned::new(conn, tcp);
        let _ = read_http_request_generic(&mut stream);
        write_http_response_generic(
            &mut stream,
            200,
            &[("content-type", "text/plain".to_string())],
            "secure",
        );
    });

    let options = crate::value::DictMap::from_iter([(
        "tls".to_string(),
        VmValue::dict(crate::value::DictMap::from_iter([
            (
                "ca_bundle_path".to_string(),
                VmValue::String(std::sync::Arc::from(cert_path.display().to_string())),
            ),
            (
                "pinned_sha256".to_string(),
                VmValue::List(std::sync::Arc::new(vec![VmValue::String(
                    std::sync::Arc::from("deadbeef"),
                )])),
            ),
        ])),
    )]);
    let error =
        vm_execute_http_request("GET", &format!("https://localhost:{port}/secure"), &options)
            .await
            .expect_err("pin mismatch should fail");
    let message = error.to_string();
    assert!(message.contains("TLS SPKI pin mismatch"), "{message}");
    thread.join().expect("tls thread");
    reset_http_state();
}

fn install_rustls_provider() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    });
}

fn spki_pin_base64(cert_der: &[u8]) -> String {
    let (_, cert) = X509Certificate::from_der(cert_der).expect("parse cert");
    base64::engine::general_purpose::STANDARD
        .encode(Sha256::digest(cert.tbs_certificate.subject_pki.raw))
}

fn read_http_request_generic<T: Read>(stream: &mut T) -> String {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let read = stream.read(&mut chunk).expect("read request");
        assert!(read > 0, "request closed before headers");
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
            return String::from_utf8_lossy(&buffer).into_owned();
        }
    }
}

fn write_http_response_generic<T: Write>(
    stream: &mut T,
    status: u16,
    headers: &[(&str, String)],
    body: &str,
) {
    let mut response = format!(
        "HTTP/1.1 {status} OK\r\ncontent-length: {}\r\nconnection: close\r\n",
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
    stream.flush().expect("flush response");
}

#[test]
fn formats_sse_event_fields_and_multiline_data() {
    let frame = vm_sse_event_frame(
        &VmValue::dict(crate::value::DictMap::from_iter([
            (
                "event".to_string(),
                VmValue::String(std::sync::Arc::from("progress")),
            ),
            (
                "data".to_string(),
                VmValue::String(std::sync::Arc::from("one\ntwo")),
            ),
            (
                "id".to_string(),
                VmValue::String(std::sync::Arc::from("evt-1")),
            ),
            ("retry_ms".to_string(), VmValue::Int(2500)),
        ])),
        &crate::value::DictMap::new(),
    )
    .expect("event frame");
    assert_eq!(
        frame,
        "id: evt-1\nevent: progress\nretry: 2500\ndata: one\ndata: two\n\n"
    );
}

#[test]
fn rejects_sse_event_control_fields_with_newlines() {
    let err = vm_sse_event_frame(
        &VmValue::dict(crate::value::DictMap::from_iter([(
            "event".to_string(),
            VmValue::String(std::sync::Arc::from("bad\nname")),
        )])),
        &crate::value::DictMap::new(),
    )
    .expect_err("newline should reject");
    assert!(err.to_string().contains("event must not contain newlines"));
}

#[test]
fn server_sse_mock_client_observes_heartbeat_disconnect_and_cancel() {
    reset_http_state();
    let response = vm_sse_server_response(&crate::value::DictMap::from_iter([(
        "max_buffered_events".to_string(),
        VmValue::Int(4),
    )]))
    .expect("response");
    let stream_id = handle_from_value(&response, "test").expect("handle");

    assert!(expect_bool(
        vm_sse_server_send(
            &stream_id,
            &VmValue::dict(crate::value::DictMap::from_iter([
                (
                    "event".to_string(),
                    VmValue::String(std::sync::Arc::from("progress")),
                ),
                (
                    "data".to_string(),
                    VmValue::String(std::sync::Arc::from("50"))
                ),
            ])),
            &crate::value::DictMap::new(),
        )
        .expect("send")
    ));
    assert!(expect_bool(
        vm_sse_server_heartbeat(
            &stream_id,
            Some(&VmValue::String(std::sync::Arc::from("tick")))
        )
        .expect("heartbeat")
    ));

    let first = vm_sse_server_mock_receive(&stream_id).expect("first");
    let first = first.as_dict().expect("first dict");
    assert_eq!(first["event"].display(), "progress");
    assert_eq!(first["data"].display(), "50");
    let heartbeat = vm_sse_server_mock_receive(&stream_id).expect("heartbeat read");
    let heartbeat = heartbeat.as_dict().expect("heartbeat dict");
    assert_eq!(heartbeat["type"].display(), "comment");
    assert_eq!(heartbeat["comment"].display(), "tick");

    assert!(expect_bool(
        vm_sse_server_mock_disconnect(&stream_id).expect("disconnect")
    ));
    assert!(expect_bool(
        vm_sse_server_observed_bool(&stream_id, "test", |handle| handle.disconnected)
            .expect("observed")
    ));
    assert!(!expect_bool(
        vm_sse_server_send(
            &stream_id,
            &VmValue::String(std::sync::Arc::from("late")),
            &crate::value::DictMap::new()
        )
        .expect("late send")
    ));

    let cancelled =
        vm_sse_server_response(&crate::value::DictMap::new()).expect("cancelled response");
    let cancelled_id = handle_from_value(&cancelled, "test").expect("cancelled handle");
    assert!(expect_bool(
        vm_sse_server_cancel(
            &cancelled_id,
            Some(&VmValue::String(std::sync::Arc::from("stop")))
        )
        .expect("cancel")
    ));
    assert!(expect_bool(
        vm_sse_server_observed_bool(&cancelled_id, "test", |handle| handle.cancelled)
            .expect("cancelled observed")
    ));
    reset_http_state();
}

#[test]
fn server_sse_rejects_oversized_events() {
    reset_http_state();
    let response = vm_sse_server_response(&crate::value::DictMap::from_iter([(
        "max_event_bytes".to_string(),
        VmValue::Int(12),
    )]))
    .expect("response");
    let stream_id = handle_from_value(&response, "test").expect("handle");
    let err = vm_sse_server_send(
        &stream_id,
        &VmValue::String(std::sync::Arc::from("this is too large")),
        &crate::value::DictMap::new(),
    )
    .expect_err("oversized event should reject");
    assert!(err.to_string().contains("max_event_bytes"));
    reset_http_state();
}

// --- SSRF egress guard: end-to-end through the real HTTP client path. ---

/// Spawn a one-shot loopback HTTP server that answers a single GET with 200.
fn spawn_loopback_ok_server() -> (u16, std::thread::JoinHandle<bool>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
    listener.set_nonblocking(false).expect("blocking listener");
    let port = listener.local_addr().expect("listener addr").port();
    let handle = std::thread::spawn(move || {
        // Best-effort: if the guard blocks the request the client never
        // connects, so accept must not hang the test forever.
        listener.set_nonblocking(false).expect("blocking listener");
        match listener.accept() {
            Ok((mut stream, _)) => {
                let _ = read_http_request_generic(&mut stream);
                write_http_response_generic(&mut stream, 200, &[], "ok");
                true
            }
            Err(_) => false,
        }
    });
    (port, handle)
}

#[tokio::test]
async fn ssrf_guard_default_on_blocks_loopback() {
    reset_http_state();
    crate::egress::reset_egress_policy_for_tests();
    let _scope = crate::egress::require_ssrf_guard_for_host();

    // No server needed: the request must be blocked before any connection.
    let error =
        vm_execute_http_request("GET", "http://127.0.0.1:9/x", &crate::value::DictMap::new())
            .await
            .expect_err("default-on blocks loopback");
    let msg = error.to_string();
    assert!(msg.contains("EgressBlocked"), "{msg}");
    assert!(msg.contains("disallowed address"), "{msg}");

    drop(_scope);
    crate::egress::reset_egress_policy_for_tests();
    reset_http_state();
}

#[tokio::test]
async fn ssrf_guard_allow_loopback_hatch_permits_capture_server() {
    reset_http_state();
    crate::egress::reset_egress_policy_for_tests();
    // Engage the guard scope (default-on), then open the loopback hatch via a
    // thread-local test policy (no process-global env mutation).
    let _scope = crate::egress::require_ssrf_guard_for_host();
    crate::egress::install_test_policy(&[
        (
            "block_private",
            VmValue::String(std::sync::Arc::from("private")),
        ),
        ("allow_loopback", VmValue::Bool(true)),
    ]);

    let (port, handle) = spawn_loopback_ok_server();
    let response = vm_execute_http_request(
        "GET",
        &format!("http://127.0.0.1:{port}/ok"),
        &crate::value::DictMap::new(),
    )
    .await
    .expect("hatch permits loopback");
    assert_eq!(
        response.as_dict().expect("response dict")["status"].as_int(),
        Some(200)
    );
    assert!(handle.join().expect("server thread"));

    drop(_scope);
    crate::egress::reset_egress_policy_for_tests();
    reset_http_state();
}

#[tokio::test]
async fn ssrf_guard_block_private_off_permits_capture_server() {
    reset_http_state();
    crate::egress::reset_egress_policy_for_tests();
    let _scope = crate::egress::require_ssrf_guard_for_host();
    // Explicit opt-out: block_private:"off" via a thread-local test policy.
    crate::egress::install_test_policy(&[(
        "block_private",
        VmValue::String(std::sync::Arc::from("off")),
    )]);

    let (port, handle) = spawn_loopback_ok_server();
    let response = vm_execute_http_request(
        "GET",
        &format!("http://127.0.0.1:{port}/ok"),
        &crate::value::DictMap::new(),
    )
    .await
    .expect("block_private:off permits loopback");
    assert_eq!(
        response.as_dict().expect("response dict")["status"].as_int(),
        Some(200)
    );
    assert!(handle.join().expect("server thread"));

    drop(_scope);
    crate::egress::reset_egress_policy_for_tests();
    reset_http_state();
}
