use super::*;
use crate::value::VmDictExt;
use std::io::{BufRead, BufReader};

#[tokio::test]
async fn both_mock_registries_record_distinct_invalid_utf8_request_bytes() {
    reset_http_state();
    let url = "https://example.test/binary-recordings";
    let registry = HttpMockRegistry::default();
    registry.register("POST", url, vec![HttpMockResponse::new(200, "ok").into()]);
    push_http_mock("POST", url, vec![HttpMockResponse::new(200, "ok")]);
    for byte in [255, 254] {
        let body = VmValue::Bytes(Arc::new(vec![byte]));
        let mut options = crate::value::DictMap::new();
        options.put("body", body.clone());
        vm_execute_http_request("POST", url, &options)
            .await
            .unwrap();
        super::super::execute_harness_http_verb(
            &registry,
            "POST",
            true,
            vec![VmValue::string(url), body],
        )
        .await
        .unwrap();
    }
    for calls in [
        super::super::mock::http_mock_calls_value(false),
        registry.calls_value(false),
    ] {
        assert_eq!(calls.len(), 2);
        assert_eq!(
            calls[0].as_dict().unwrap()["body"].display(),
            calls[1].as_dict().unwrap()["body"].display()
        );
        for (call, expected) in calls.iter().zip([255, 254]) {
            assert_eq!(
                call.as_dict().unwrap()["body_bytes"].as_bytes(),
                Some([expected].as_slice())
            );
        }
    }
    let snapshots = super::super::http_mock_calls_snapshot();
    assert_eq!(snapshots.len(), 2);
    assert_eq!(snapshots[0].body, snapshots[1].body);
    assert_eq!(
        snapshots[0],
        super::super::HttpMockCallSnapshot {
            method: "POST".to_string(),
            url: url.to_string(),
            headers: std::collections::BTreeMap::new(),
            body: Some("\u{fffd}".to_string()),
        }
    );
    reset_http_state();
}

fn binary_server(
    requests: usize,
    content_length: bool,
) -> (String, std::thread::JoinHandle<Vec<Vec<u8>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let worker = crate::runtime_stack::spawn(move || {
        let mut bodies = Vec::new();
        for _ in 0..requests {
            let (mut stream, _) = listener.accept().unwrap();
            let mut input = BufReader::new(stream.try_clone().unwrap());
            let mut length = 0;
            loop {
                let mut line = String::new();
                assert!(input.read_line(&mut line).unwrap() > 0);
                if line == "\r\n" {
                    break;
                }
                if let Some((name, value)) = line.split_once(':') {
                    if name.eq_ignore_ascii_case("content-length") {
                        length = value.trim().parse().unwrap();
                    }
                }
            }
            let mut body = vec![0; length];
            input.read_exact(&mut body).unwrap();
            bodies.push(body);
            let header = if content_length {
                "content-length: 2\r\n"
            } else {
                ""
            };
            let mut response = format!("HTTP/1.1 200 OK\r\ncontent-type: application/octet-stream\r\n{header}connection: close\r\n\r\n").into_bytes();
            response.extend_from_slice(&[0, 255]);
            stream.write_all(&response).unwrap();
        }
        bodies
    });
    (format!("http://{address}/binary"), worker)
}

#[tokio::test]
async fn native_buffered_and_positional_requests_round_trip_non_utf8_bytes() {
    reset_http_state();
    let (url, worker) = binary_server(2, true);
    let raw = VmValue::Bytes(Arc::new(vec![0, 255]));
    let mut options = crate::value::DictMap::new();
    options.put_str("response_type", "bytes");
    options.put("body", raw.clone());
    let direct = vm_execute_http_request("POST", &url, &options)
        .await
        .unwrap();
    assert_eq!(
        direct
            .as_dict()
            .unwrap()
            .get("body_bytes")
            .unwrap()
            .as_bytes(),
        Some([0, 255].as_slice())
    );
    options.remove("body");
    let positional = super::super::execute_harness_http_verb(
        &HttpMockRegistry::default(),
        "POST",
        true,
        vec![VmValue::string(&url), raw, VmValue::dict(options)],
    )
    .await
    .unwrap();
    assert_eq!(
        positional
            .as_dict()
            .unwrap()
            .get("body_bytes")
            .unwrap()
            .as_bytes(),
        Some([0, 255].as_slice())
    );
    assert_eq!(worker.join().unwrap(), vec![vec![0, 255], vec![0, 255]]);
    reset_http_state();
}

#[tokio::test]
async fn binary_responses_keep_text_validation_and_both_buffer_limits() {
    reset_http_state();
    let (url, worker) = binary_server(1, true);
    let error = vm_execute_http_request("GET", &url, &crate::value::DictMap::new())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("not valid UTF-8"));
    assert_eq!(worker.join().unwrap(), vec![Vec::<u8>::new()]);
    for declared in [true, false] {
        let (url, worker) = binary_server(1, declared);
        let mut options = crate::value::DictMap::new();
        options.put_str("response_type", "bytes");
        options.put("max_response_bytes", VmValue::Int(1));
        let error = vm_execute_http_request("GET", &url, &options)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("exceeded max_response_bytes"));
        assert_eq!(worker.join().unwrap(), vec![Vec::<u8>::new()]);
    }
    reset_http_state();
}

#[test]
fn request_parts_preserve_binary_and_text_bodies() {
    for (body, expected) in [
        (
            VmValue::Bytes(std::sync::Arc::new(vec![0, 255])),
            vec![0, 255],
        ),
        (VmValue::string("text"), b"text".to_vec()),
    ] {
        let mut options = crate::value::DictMap::new();
        options.put("body", body);
        let parts = super::super::client::parse_http_request_parts("POST", &options).unwrap();
        assert_eq!(parts.body.as_deref(), Some(expected.as_slice()));
    }
}

#[test]
fn multipart_headers_are_preserved_or_refused_without_echoing_values() {
    let options = crate::value::json::json_to_vm_value(&serde_json::json!({
        "multipart": [{"name":"file", "value_base64":"AP8=", "headers":{"X-Part-Tag":"public-control"}}]
    }));
    let parts =
        super::super::client::parse_http_request_parts("POST", options.as_dict().unwrap()).unwrap();
    assert!(String::from_utf8_lossy(parts.body.as_ref().unwrap())
        .contains("x-part-tag: public-control\r\n"));
    for headers in [
        serde_json::json!({"Content-Type":"private-fixture-value"}),
        serde_json::json!({"Content-Disposition":"private-fixture-value"}),
        serde_json::json!({"X-Part":"private-fixture-value\r\nextra"}),
        serde_json::json!({"bad name":"private-fixture-value"}),
        serde_json::json!({"X-Part":"first", "x-part":"private-fixture-value"}),
        serde_json::json!({"X-Part":42}),
    ] {
        let options = crate::value::json::json_to_vm_value(&serde_json::json!({
            "multipart": [{"name":"file", "value_base64":"AP8=", "headers":headers}]
        }));
        let error =
            super::super::client::parse_http_request_parts("POST", options.as_dict().unwrap())
                .err()
                .expect("invalid headers must refuse");
        assert!(error.to_string().contains("multipart"));
        assert!(!error.to_string().contains("private-fixture-value"));
    }
}

#[tokio::test]
async fn binary_response_mode_is_explicit_and_mocked_text_is_unchanged() {
    reset_http_state();
    push_http_mock(
        "GET",
        "https://example.test/bytes",
        vec![HttpMockResponse::new(200, "text")],
    );
    let mut options = crate::value::DictMap::new();
    options.put_str("response_type", "bytes");
    let response = vm_execute_http_request("GET", "https://example.test/bytes", &options)
        .await
        .unwrap();
    let response = response.as_dict().unwrap();
    assert!(matches!(response.get("body"), Some(VmValue::String(value)) if value.as_str() == ""));
    assert_eq!(
        response.get("body_bytes").unwrap().as_bytes(),
        Some(b"text".as_slice())
    );
    push_http_mock(
        "GET",
        "https://example.test/text",
        vec![HttpMockResponse::new(200, "text")],
    );
    let text = vm_execute_http_request(
        "GET",
        "https://example.test/text",
        &crate::value::DictMap::new(),
    )
    .await
    .unwrap();
    let text = text.as_dict().unwrap();
    assert!(matches!(text.get("body"), Some(VmValue::String(value)) if value.as_str() == "text"));
    assert!(!text.contains_key("body_bytes"));
    reset_http_state();
}

#[tokio::test]
async fn invalid_response_mode_refuses_before_connecting_and_does_not_echo_values() {
    let mut options = crate::value::DictMap::new();
    options.put_str("response_type", "private-fixture-mode");
    let error = vm_execute_http_request("GET", "http://127.0.0.1:9/", &options)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("response_type must be text or bytes"));
    assert!(!error.contains("private-fixture-mode"));
}
