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
use crate::connectors::test_util::{spawn_mock_http_server, write_http_response};
use crate::value::VmValue;
use base64::Engine;
use rcgen::generate_simple_self_signed;
use rustls::pki_types::PrivatePkcs8KeyDer;
use rustls::{ServerConfig, ServerConnection, StreamOwned};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::rc::Rc;
use std::sync::{Arc, Once};
use std::time::{Duration, SystemTime};
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
    let header = httpdate::fmt_http_date(SystemTime::now() + Duration::from_secs(2));
    let parsed = parse_retry_after_value(&header).expect("http-date should parse");
    assert!(parsed <= Duration::from_secs(2));
    assert!(parsed <= Duration::from_secs(60));
}

#[test]
fn malformed_retry_after_returns_none() {
    assert_eq!(parse_retry_after_value("soon-ish"), None);
}

#[test]
fn retry_delay_honors_retry_after_floor() {
    let delay = compute_retry_delay(0, 1, Some(Duration::from_millis(250)));
    assert!(delay >= Duration::from_millis(250));
    assert!(delay <= Duration::from_secs(60));
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
        &BTreeMap::from([
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
    let options = BTreeMap::from([
        (
            "headers".to_string(),
            VmValue::Dict(Rc::new(BTreeMap::from([
                (
                    "Authorization".to_string(),
                    VmValue::String(Rc::from("Bearer secret")),
                ),
                ("X-Trace".to_string(), VmValue::String(Rc::from("trace-1"))),
            ]))),
        ),
        (
            "query".to_string(),
            VmValue::Dict(Rc::new(BTreeMap::from([
                ("api_key".to_string(), VmValue::String(Rc::from("secret"))),
                ("limit".to_string(), VmValue::Int(2)),
            ]))),
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
    let headers = BTreeMap::from([
        (
            "authorization".to_string(),
            VmValue::String(Rc::from("Bearer secret")),
        ),
        (
            "accept".to_string(),
            VmValue::String(Rc::from("application/json")),
        ),
        ("x-api-key".to_string(), VmValue::String(Rc::from("secret"))),
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
    let options = BTreeMap::from([(
        "multipart".to_string(),
        VmValue::List(Rc::new(vec![
            VmValue::Dict(Rc::new(BTreeMap::from([
                ("name".to_string(), VmValue::String(Rc::from("meta"))),
                ("value".to_string(), VmValue::String(Rc::from("hello"))),
            ]))),
            VmValue::Dict(Rc::new(BTreeMap::from([
                ("name".to_string(), VmValue::String(Rc::from("blob"))),
                (
                    "filename".to_string(),
                    VmValue::String(Rc::from("blob.bin")),
                ),
                (
                    "content_type".to_string(),
                    VmValue::String(Rc::from("application/octet-stream")),
                ),
                (
                    "value".to_string(),
                    VmValue::Bytes(Rc::new(vec![0, 1, 2, 3])),
                ),
            ]))),
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

    let handle = vm_http_stream_open("https://api.example.com/stream", &BTreeMap::new())
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
        &BTreeMap::new(),
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
async fn http_proxy_routes_requests_through_configured_proxy() {
    reset_http_state();
    let proxy = spawn_mock_http_server(1, "proxy listener", |_index, _addr, request, stream| {
        assert_eq!(request.method, "GET");
        assert_eq!(request.path, "http://example.invalid/proxy-check");
        assert_eq!(
            request
                .headers
                .get("proxy-authorization")
                .map(String::as_str),
            Some("Basic dXNlcjpwYXNz")
        );
        write_http_response(
            stream,
            200,
            &[("content-type", "text/plain".to_string())],
            "proxied",
        );
    });

    let options = BTreeMap::from([
        (
            "proxy".to_string(),
            VmValue::String(Rc::from(proxy.base_url().to_string())),
        ),
        (
            "proxy_auth".to_string(),
            VmValue::Dict(Rc::new(BTreeMap::from([
                ("user".to_string(), VmValue::String(Rc::from("user"))),
                ("pass".to_string(), VmValue::String(Rc::from("pass"))),
            ]))),
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
                PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der()).into(),
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

    let options = BTreeMap::from([(
        "tls".to_string(),
        VmValue::Dict(Rc::new(BTreeMap::from([
            (
                "ca_bundle_path".to_string(),
                VmValue::String(Rc::from(cert_path.display().to_string())),
            ),
            (
                "pinned_sha256".to_string(),
                VmValue::List(Rc::new(vec![VmValue::String(Rc::from(pin))])),
            ),
        ]))),
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
                PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der()).into(),
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

    let options = BTreeMap::from([(
        "tls".to_string(),
        VmValue::Dict(Rc::new(BTreeMap::from([
            (
                "ca_bundle_path".to_string(),
                VmValue::String(Rc::from(cert_path.display().to_string())),
            ),
            (
                "pinned_sha256".to_string(),
                VmValue::List(Rc::new(vec![VmValue::String(Rc::from("deadbeef"))])),
            ),
        ]))),
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
        &VmValue::Dict(Rc::new(BTreeMap::from([
            ("event".to_string(), VmValue::String(Rc::from("progress"))),
            ("data".to_string(), VmValue::String(Rc::from("one\ntwo"))),
            ("id".to_string(), VmValue::String(Rc::from("evt-1"))),
            ("retry_ms".to_string(), VmValue::Int(2500)),
        ]))),
        &BTreeMap::new(),
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
        &VmValue::Dict(Rc::new(BTreeMap::from([(
            "event".to_string(),
            VmValue::String(Rc::from("bad\nname")),
        )]))),
        &BTreeMap::new(),
    )
    .expect_err("newline should reject");
    assert!(err.to_string().contains("event must not contain newlines"));
}

#[test]
fn server_sse_mock_client_observes_heartbeat_disconnect_and_cancel() {
    reset_http_state();
    let response = vm_sse_server_response(&BTreeMap::from([(
        "max_buffered_events".to_string(),
        VmValue::Int(4),
    )]))
    .expect("response");
    let stream_id = handle_from_value(&response, "test").expect("handle");

    assert!(expect_bool(
        vm_sse_server_send(
            &stream_id,
            &VmValue::Dict(Rc::new(BTreeMap::from([
                ("event".to_string(), VmValue::String(Rc::from("progress")),),
                ("data".to_string(), VmValue::String(Rc::from("50"))),
            ]))),
            &BTreeMap::new(),
        )
        .expect("send")
    ));
    assert!(expect_bool(
        vm_sse_server_heartbeat(&stream_id, Some(&VmValue::String(Rc::from("tick"))))
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
            &VmValue::String(Rc::from("late")),
            &BTreeMap::new()
        )
        .expect("late send")
    ));

    let cancelled = vm_sse_server_response(&BTreeMap::new()).expect("cancelled response");
    let cancelled_id = handle_from_value(&cancelled, "test").expect("cancelled handle");
    assert!(expect_bool(
        vm_sse_server_cancel(&cancelled_id, Some(&VmValue::String(Rc::from("stop"))))
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
    let response = vm_sse_server_response(&BTreeMap::from([(
        "max_event_bytes".to_string(),
        VmValue::Int(12),
    )]))
    .expect("response");
    let stream_id = handle_from_value(&response, "test").expect("handle");
    let err = vm_sse_server_send(
        &stream_id,
        &VmValue::String(Rc::from("this is too large")),
        &BTreeMap::new(),
    )
    .expect_err("oversized event should reject");
    assert!(err.to_string().contains("max_event_bytes"));
    reset_http_state();
}
