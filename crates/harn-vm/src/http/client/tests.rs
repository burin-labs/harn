use super::*;

fn proxy_options(password: &str) -> crate::value::DictMap {
    crate::value::DictMap::from_iter([
        (
            crate::value::intern_key("proxy"),
            VmValue::string("http://proxy.local:8080"),
        ),
        (
            crate::value::intern_key("proxy_auth"),
            VmValue::dict([
                ("user", VmValue::string("alice")),
                ("pass", VmValue::string(password)),
            ]),
        ),
    ])
}

#[test]
fn http_client_key_distinguishes_proxy_password_without_leaking_it() {
    let first = http_client_key(&parse_http_options(&proxy_options("first-secret")));
    let second = http_client_key(&parse_http_options(&proxy_options("second-secret")));

    assert_ne!(first, second);
    assert!(!first.contains("first-secret"));
    assert!(!second.contains("second-secret"));
}

#[test]
fn response_body_limit_rejects_oversized_mock_body() {
    let error = ensure_response_body_within_limit(11, 10).unwrap_err();
    assert!(error
        .to_string()
        .contains("response body exceeded max_response_bytes"));
}

#[test]
fn download_response_saturates_large_byte_count() {
    let response = build_http_download_response(200, crate::value::DictMap::new(), u64::MAX);
    let response = response.as_dict().expect("response dict");
    assert_eq!(response["bytes_written"].as_int(), Some(i64::MAX));
}
