use super::*;
use crate::llm::helpers::vm_value_to_json;

fn dict(value: &VmValue) -> &crate::value::DictMap {
    value.as_dict().expect("envelope is a dict")
}

fn run_sync<F, Fut>(future: F) -> Fut::Output
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future,
{
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("rt")
        .block_on(future())
}

#[test]
fn http_ok_produces_tagged_envelope() {
    let body = VmValue::String(arcstr::ArcStr::from("hello"));
    let response = http_ok_impl(&[body], &mut String::new()).expect("ok");
    let map = dict(&response);
    assert_eq!(
        map.get(HTTP_RESPONSE_TAG_KEY).and_then(|v| match v {
            VmValue::String(s) => Some(s.as_str()),
            _ => None,
        }),
        Some(HTTP_RESPONSE_TAG_VERSION)
    );
    assert!(matches!(map.get("status"), Some(VmValue::Int(200))));
    assert_eq!(
        map.get("body").map(|v| v.display()).as_deref(),
        Some("hello")
    );
}

#[test]
fn http_created_sets_location_header() {
    let body = VmValue::dict(crate::value::DictMap::from_iter([(
        crate::value::intern_key("id"),
        VmValue::String(arcstr::ArcStr::from("sess_1")),
    )]));
    let location = VmValue::String(arcstr::ArcStr::from("/v1/sessions/sess_1"));
    let response = http_created_impl(&[body, location], &mut String::new()).expect("created");
    let map = dict(&response);
    assert!(matches!(map.get("status"), Some(VmValue::Int(201))));
    let headers = map
        .get("headers")
        .and_then(VmValue::as_dict)
        .expect("headers");
    assert_eq!(
        headers.get("Location").map(|v| v.display()).as_deref(),
        Some("/v1/sessions/sess_1")
    );
}

#[test]
fn http_no_content_omits_body_marker() {
    let response = http_no_content_impl(&[], &mut String::new()).expect("no_content");
    let map = dict(&response);
    assert!(matches!(map.get("status"), Some(VmValue::Int(204))));
    assert!(map.get("body").is_none());
    assert_eq!(
        map.get("body_kind").and_then(|v| match v {
            VmValue::String(s) => Some(s.as_str()),
            _ => None,
        }),
        Some(BODY_KIND_NONE)
    );
}

#[test]
fn http_error_carries_code_message_and_marker() {
    let response = http_error_impl(
        &[
            VmValue::Int(422),
            VmValue::String(arcstr::ArcStr::from("invalid_input")),
            VmValue::String(arcstr::ArcStr::from("bad payload")),
            VmValue::Nil,
        ],
        &mut String::new(),
    )
    .expect("error");
    let map = dict(&response);
    assert!(matches!(map.get("status"), Some(VmValue::Int(422))));
    assert!(matches!(map.get("is_error"), Some(VmValue::Bool(true))));
    let body = map
        .get("body")
        .and_then(VmValue::as_dict)
        .expect("body dict");
    assert_eq!(
        body.get("code").map(|v| v.display()).as_deref(),
        Some("invalid_input")
    );
    assert_eq!(
        body.get("message").map(|v| v.display()).as_deref(),
        Some("bad payload")
    );
}

#[test]
fn http_error_rejects_2xx_status() {
    let err = http_error_impl(
        &[
            VmValue::Int(200),
            VmValue::String(arcstr::ArcStr::from("x")),
            VmValue::String(arcstr::ArcStr::from("y")),
        ],
        &mut String::new(),
    )
    .expect_err("expected reject");
    match err {
        VmError::Thrown(VmValue::String(text)) => {
            assert!(text.contains("4xx or 5xx"), "got: {text}");
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn http_reply_rejects_out_of_range_status() {
    let err = http_reply_impl(&[VmValue::Int(999)], &mut String::new()).expect_err("out of range");
    match err {
        VmError::Thrown(VmValue::String(text)) => {
            assert!(text.contains("100-599"), "got: {text}");
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn http_reply_bytes_uses_bytes_body_kind() {
    let bytes = VmValue::Bytes(std::sync::Arc::new(vec![0x00, 0xff, 0xfe, 0x80]));
    let headers = VmValue::dict(crate::value::DictMap::from_iter([(
        crate::value::intern_key("Content-Type"),
        VmValue::String(arcstr::ArcStr::from("application/octet-stream")),
    )]));
    let response =
        http_reply_impl(&[VmValue::Int(200), bytes, headers], &mut String::new()).unwrap();
    let map = dict(&response);
    assert_eq!(
        map.get("body_kind").and_then(|v| match v {
            VmValue::String(s) => Some(s.as_str()),
            _ => None,
        }),
        Some(BODY_KIND_BYTES)
    );
    assert!(matches!(map.get("body"), Some(VmValue::Bytes(_))));
}

#[test]
fn http_reply_from_wraps_stream_body_as_chunk_list() {
    let result = VmValue::dict(crate::value::DictMap::from_iter([
        (crate::value::intern_key("status"), VmValue::Int(202)),
        (
            crate::value::intern_key("body_kind"),
            VmValue::string("stream"),
        ),
        (
            crate::value::intern_key("headers"),
            VmValue::dict(crate::value::DictMap::from_iter([(
                crate::value::intern_key("Content-Type"),
                VmValue::string("text/plain"),
            )])),
        ),
        (crate::value::intern_key("body"), VmValue::string("queued")),
    ]));

    let response = http_reply_from_impl(&[result], &mut String::new()).unwrap();
    let map = dict(&response);
    assert!(matches!(map.get("status"), Some(VmValue::Int(202))));
    assert_eq!(
        map.get("body_kind").and_then(|v| match v {
            VmValue::String(s) => Some(s.as_str()),
            _ => None,
        }),
        Some(BODY_KIND_STREAM)
    );
    let body = match map.get("body") {
        Some(VmValue::List(items)) => items,
        other => panic!("expected stream body chunk list, got {other:?}"),
    };
    assert_eq!(body.len(), 1);
    assert_eq!(body[0].display(), "queued");
}

#[test]
fn http_reply_from_preserves_existing_stream_chunks() {
    let chunks = VmValue::List(std::sync::Arc::new(vec![
        VmValue::string("alpha"),
        VmValue::string("bravo"),
    ]));
    let result = VmValue::dict(crate::value::DictMap::from_iter([
        (crate::value::intern_key("status"), VmValue::Int(200)),
        (
            crate::value::intern_key("body_kind"),
            VmValue::string("stream"),
        ),
        (crate::value::intern_key("body"), chunks),
    ]));

    let response = http_reply_from_impl(&[result], &mut String::new()).unwrap();
    let map = dict(&response);
    let body = match map.get("body") {
        Some(VmValue::List(items)) => items,
        other => panic!("expected stream body chunk list, got {other:?}"),
    };
    assert_eq!(body.len(), 2);
    assert_eq!(body[0].display(), "alpha");
    assert_eq!(body[1].display(), "bravo");
}

#[test]
fn http_reply_from_preserves_raw_body_for_bytes_kind() {
    let raw = VmValue::Bytes(std::sync::Arc::new(vec![0x00, 0xff, 0xfe, 0x80]));
    let result = VmValue::dict(crate::value::DictMap::from_iter([
        (crate::value::intern_key("status"), VmValue::Int(200)),
        (
            crate::value::intern_key("body_kind"),
            VmValue::string("bytes"),
        ),
        (crate::value::intern_key("body"), VmValue::string("<lossy>")),
        (crate::value::intern_key("raw_body"), raw),
    ]));

    let response = http_reply_from_impl(&[result], &mut String::new()).unwrap();
    let map = dict(&response);
    assert_eq!(
        map.get("body_kind").and_then(|v| match v {
            VmValue::String(s) => Some(s.as_str()),
            _ => None,
        }),
        Some(BODY_KIND_BYTES)
    );
    match map.get("body") {
        Some(VmValue::Bytes(bytes)) => assert_eq!(bytes.as_ref(), &[0x00, 0xff, 0xfe, 0x80]),
        other => panic!("expected bytes body, got {other:?}"),
    }
}

#[test]
fn http_reply_from_rejects_non_bytes_for_bytes_kind() {
    let result = VmValue::dict(crate::value::DictMap::from_iter([
        (crate::value::intern_key("status"), VmValue::Int(200)),
        (
            crate::value::intern_key("body_kind"),
            VmValue::string("bytes"),
        ),
        (
            crate::value::intern_key("body"),
            VmValue::string("not bytes"),
        ),
    ]));

    let err =
        http_reply_from_impl(&[result], &mut String::new()).expect_err("expected bytes error");
    match err {
        VmError::Thrown(VmValue::String(text)) => {
            assert!(text.contains("requires bytes"), "unexpected error: {text}");
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn http_reply_from_falls_back_to_http_reply_for_text_kind() {
    let result = VmValue::dict(crate::value::DictMap::from_iter([
        (crate::value::intern_key("status"), VmValue::Int(200)),
        (
            crate::value::intern_key("body_kind"),
            VmValue::string("text"),
        ),
        (crate::value::intern_key("body"), VmValue::string("hello")),
    ]));

    let response = http_reply_from_impl(&[result], &mut String::new()).unwrap();
    let map = dict(&response);
    assert_eq!(
        map.get("body_kind").and_then(|v| match v {
            VmValue::String(s) => Some(s.as_str()),
            _ => None,
        }),
        Some(BODY_KIND_JSON)
    );
    assert_eq!(
        map.get("body").map(VmValue::display).as_deref(),
        Some("hello")
    );
}

#[test]
fn http_reply_from_rejects_non_dict_result() {
    let err = http_reply_from_impl(&[VmValue::string("nope")], &mut String::new())
        .expect_err("expected result type error");
    match err {
        VmError::Thrown(VmValue::String(text)) => {
            assert!(
                text.contains("result must be a dict"),
                "unexpected error: {text}"
            );
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn http_stream_buffers_list_source() {
    let items = vec![
        VmValue::String(arcstr::ArcStr::from("a")),
        VmValue::String(arcstr::ArcStr::from("b")),
    ];
    let response = run_sync(|| {
        http_stream_impl(
            crate::vm::AsyncBuiltinCtx::for_test(Vm::new()),
            vec![
                VmValue::List(std::sync::Arc::new(items.clone())),
                VmValue::String(arcstr::ArcStr::from("text/plain")),
            ],
        )
    })
    .expect("stream");
    let map = dict(&response);
    assert_eq!(
        map.get("body_kind").and_then(|v| match v {
            VmValue::String(s) => Some(s.as_str()),
            _ => None,
        }),
        Some(BODY_KIND_STREAM)
    );
    let body = map.get("body").expect("body");
    match body {
        VmValue::List(values) => {
            assert_eq!(values.len(), 2);
        }
        other => panic!("expected list body, got {other:?}"),
    }
    let headers = map
        .get("headers")
        .and_then(VmValue::as_dict)
        .expect("headers");
    assert_eq!(
        headers.get("Content-Type").map(|v| v.display()).as_deref(),
        Some("text/plain")
    );
}

#[test]
fn http_sse_sets_event_stream_headers_and_optional_retry() {
    let events = vec![VmValue::dict(crate::value::DictMap::from_iter([(
        crate::value::intern_key("data"),
        VmValue::String(arcstr::ArcStr::from("ping")),
    )]))];
    let response = run_sync(|| {
        http_sse_impl(
            crate::vm::AsyncBuiltinCtx::for_test(Vm::new()),
            vec![
                VmValue::List(std::sync::Arc::new(events.clone())),
                VmValue::Int(2500),
            ],
        )
    })
    .expect("sse");
    let map = dict(&response);
    let headers = map
        .get("headers")
        .and_then(VmValue::as_dict)
        .expect("headers");
    assert_eq!(
        headers.get("Content-Type").map(|v| v.display()).as_deref(),
        Some("text/event-stream")
    );
    assert_eq!(
        headers.get("Cache-Control").map(|v| v.display()).as_deref(),
        Some("no-cache")
    );
    assert!(matches!(map.get("retry_ms"), Some(VmValue::Int(2500))));
}

#[test]
fn parse_envelope_round_trip_through_json() {
    let response = http_error_impl(
        &[
            VmValue::Int(404),
            VmValue::String(arcstr::ArcStr::from("not_found")),
            VmValue::String(arcstr::ArcStr::from("missing")),
            VmValue::dict(crate::value::DictMap::from_iter([(
                crate::value::intern_key("id"),
                VmValue::String(arcstr::ArcStr::from("sess_404")),
            )])),
        ],
        &mut String::new(),
    )
    .expect("error");
    let json = vm_value_to_json(&response);
    let envelope = parse_envelope(&json).expect("envelope parses");
    assert_eq!(envelope.status, 404);
    assert!(envelope.is_error);
    let body = envelope.body.expect("body");
    assert_eq!(body["code"], "not_found");
    assert_eq!(body["details"]["id"], "sess_404");
}

#[test]
fn parse_envelope_ignores_untagged_dicts() {
    let plain = serde_json::json!({"status": 200, "body": {}});
    assert!(parse_envelope(&plain).is_none());
}

#[test]
fn http_etag_is_quoted_hex_sha256_of_payload() {
    let value = VmValue::String(arcstr::ArcStr::from("hello"));
    let etag = http_etag_impl(&[value], &mut String::new()).expect("etag");
    match etag {
        VmValue::String(text) => {
            assert_eq!(
                text.as_str(),
                "\"2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824\""
            );
        }
        other => panic!("expected string, got {other:?}"),
    }
}

#[test]
fn http_etag_stable_across_string_and_bytes_for_same_payload() {
    let from_string = http_etag_impl(
        &[VmValue::String(arcstr::ArcStr::from("hello"))],
        &mut String::new(),
    )
    .unwrap();
    let from_bytes = http_etag_impl(
        &[VmValue::Bytes(std::sync::Arc::new(b"hello".to_vec()))],
        &mut String::new(),
    )
    .unwrap();
    assert_eq!(from_string.display(), from_bytes.display());
}

#[test]
fn http_choose_returns_best_q_match() {
    let accept = VmValue::String(arcstr::ArcStr::from(
        "application/xml;q=0.5, application/json;q=0.9",
    ));
    let offers = VmValue::List(std::sync::Arc::new(vec![
        VmValue::String(arcstr::ArcStr::from("application/xml")),
        VmValue::String(arcstr::ArcStr::from("application/json")),
    ]));
    let chosen = http_choose_impl(&[accept, offers], &mut String::new()).unwrap();
    assert_eq!(chosen.display(), "application/json");
}

#[test]
fn http_choose_prefers_specific_over_wildcard() {
    let accept = VmValue::String(arcstr::ArcStr::from("text/*;q=0.5, application/json"));
    let offers = VmValue::List(std::sync::Arc::new(vec![
        VmValue::String(arcstr::ArcStr::from("text/plain")),
        VmValue::String(arcstr::ArcStr::from("application/json")),
    ]));
    let chosen = http_choose_impl(&[accept, offers], &mut String::new()).unwrap();
    assert_eq!(chosen.display(), "application/json");
}

#[test]
fn http_choose_returns_default_for_no_accept() {
    let offers = VmValue::List(std::sync::Arc::new(vec![
        VmValue::String(arcstr::ArcStr::from("text/plain")),
        VmValue::String(arcstr::ArcStr::from("application/json")),
    ]));
    let chosen = http_choose_impl(&[VmValue::Nil, offers], &mut String::new()).unwrap();
    assert_eq!(chosen.display(), "text/plain");
}

#[test]
fn http_choose_overrides_default_with_explicit() {
    let offers = VmValue::List(std::sync::Arc::new(vec![
        VmValue::String(arcstr::ArcStr::from("text/plain")),
        VmValue::String(arcstr::ArcStr::from("application/json")),
    ]));
    let chosen = http_choose_impl(
        &[
            VmValue::Nil,
            offers,
            VmValue::String(arcstr::ArcStr::from("application/json")),
        ],
        &mut String::new(),
    )
    .unwrap();
    assert_eq!(chosen.display(), "application/json");
}

#[test]
fn http_choose_wildcard_accept_yields_default() {
    let offers = VmValue::List(std::sync::Arc::new(vec![VmValue::String(
        arcstr::ArcStr::from("application/json"),
    )]));
    let chosen = http_choose_impl(
        &[VmValue::String(arcstr::ArcStr::from("*/*")), offers],
        &mut String::new(),
    )
    .unwrap();
    assert_eq!(chosen.display(), "application/json");
}

#[test]
fn http_not_modified_envelope_carries_etag() {
    let etag = VmValue::String(arcstr::ArcStr::from("\"abc\""));
    let response = http_not_modified_impl(&[etag, VmValue::Nil], &mut String::new()).unwrap();
    let map = dict(&response);
    assert!(matches!(map.get("status"), Some(VmValue::Int(304))));
    let headers = map
        .get("headers")
        .and_then(VmValue::as_dict)
        .expect("headers");
    assert_eq!(
        headers.get("ETag").map(|v| v.display()).as_deref(),
        Some("\"abc\"")
    );
}

#[test]
fn http_push_hints_appends_link_headers_with_inferred_as() {
    let envelope = http_ok_impl(
        &[VmValue::dict(crate::value::DictMap::new())],
        &mut String::new(),
    )
    .unwrap();
    let paths = VmValue::List(std::sync::Arc::new(vec![
        VmValue::String(arcstr::ArcStr::from("/main.css")),
        VmValue::String(arcstr::ArcStr::from("/app.js")),
        VmValue::String(arcstr::ArcStr::from("/hero.webp")),
        VmValue::String(arcstr::ArcStr::from("/inter.woff2")),
        VmValue::String(arcstr::ArcStr::from("/manifest.json")),
        VmValue::String(arcstr::ArcStr::from("/unknown.xyz")),
    ]));
    let response =
        http_push_hints_impl(&[envelope, paths], &mut String::new()).expect("push_hints");
    let map = dict(&response);
    let headers = map
        .get("headers")
        .and_then(VmValue::as_dict)
        .expect("headers");
    let links = match headers.get("Link") {
        Some(VmValue::List(items)) => items.clone(),
        other => panic!("Link should be a list, got {other:?}"),
    };
    let rendered: Vec<String> = links
        .iter()
        .map(|v| match v {
            VmValue::String(s) => s.to_string(),
            other => panic!("Link entry is not a string: {other:?}"),
        })
        .collect();
    assert_eq!(
        rendered,
        vec![
            "</main.css>; rel=preload; as=style",
            "</app.js>; rel=preload; as=script",
            "</hero.webp>; rel=preload; as=image",
            "</inter.woff2>; rel=preload; as=font",
            "</manifest.json>; rel=preload; as=fetch",
            "</unknown.xyz>; rel=preload",
        ]
    );
}

#[test]
fn http_push_hints_handles_querystring_in_path() {
    let envelope = http_ok_impl(&[VmValue::Nil], &mut String::new()).unwrap();
    let paths = VmValue::List(std::sync::Arc::new(vec![VmValue::String(
        arcstr::ArcStr::from("/static/app.js?v=42"),
    )]));
    let response =
        http_push_hints_impl(&[envelope, paths], &mut String::new()).expect("push_hints");
    let map = dict(&response);
    let headers = map
        .get("headers")
        .and_then(VmValue::as_dict)
        .expect("headers");
    let links = match headers.get("Link") {
        Some(VmValue::List(items)) => items.clone(),
        other => panic!("Link should be a list, got {other:?}"),
    };
    assert_eq!(
        links[0].display(),
        "</static/app.js?v=42>; rel=preload; as=script"
    );
}

#[test]
fn http_push_hints_rejects_untagged_envelope() {
    let plain = VmValue::dict(crate::value::DictMap::from_iter([(
        crate::value::intern_key("status"),
        VmValue::Int(200),
    )]));
    let paths = VmValue::List(std::sync::Arc::new(vec![VmValue::String(
        arcstr::ArcStr::from("/main.css"),
    )]));
    let result = http_push_hints_impl(&[plain, paths], &mut String::new());
    assert!(
        matches!(result, Err(VmError::Thrown(_))),
        "untagged dict should be rejected, got {result:?}"
    );
}

#[test]
fn http_push_hints_preserves_existing_link_header() {
    let envelope = http_reply_impl(
        &[
            VmValue::Int(200),
            VmValue::dict(crate::value::DictMap::new()),
            VmValue::dict(crate::value::DictMap::from_iter([(
                crate::value::intern_key("Link"),
                VmValue::String(arcstr::ArcStr::from("</legacy.css>; rel=preload; as=style")),
            )])),
        ],
        &mut String::new(),
    )
    .unwrap();
    let paths = VmValue::List(std::sync::Arc::new(vec![VmValue::String(
        arcstr::ArcStr::from("/app.js"),
    )]));
    let response = http_push_hints_impl(&[envelope, paths], &mut String::new()).unwrap();
    let map = dict(&response);
    let headers = map
        .get("headers")
        .and_then(VmValue::as_dict)
        .expect("headers");
    let links = match headers.get("Link") {
        Some(VmValue::List(items)) => items.clone(),
        other => panic!("Link should be a list once preloads are added, got {other:?}"),
    };
    assert_eq!(links.len(), 2);
    assert_eq!(links[0].display(), "</legacy.css>; rel=preload; as=style");
    assert_eq!(links[1].display(), "</app.js>; rel=preload; as=script");
}

#[test]
fn http_upgrade_ws_envelope_negotiates_subprotocol() {
    let req = VmValue::dict(crate::value::DictMap::from_iter([(
        crate::value::intern_key("headers"),
        VmValue::dict(crate::value::DictMap::from_iter([(
            crate::value::intern_key("Sec-WebSocket-Protocol"),
            VmValue::String(arcstr::ArcStr::from("v0.harn, v1.harn")),
        )])),
    )]));
    let options = VmValue::dict(crate::value::DictMap::from_iter([(
        crate::value::intern_key("subprotocols"),
        VmValue::List(std::sync::Arc::new(vec![
            VmValue::String(arcstr::ArcStr::from("v1.harn")),
            VmValue::String(arcstr::ArcStr::from("v2.harn")),
        ])),
    )]));
    let response = http_upgrade_ws_impl(&[req, options], &mut String::new()).unwrap();
    let map = dict(&response);
    assert!(matches!(map.get("status"), Some(VmValue::Int(101))));
    let upgrade = map
        .get("ws_upgrade")
        .and_then(VmValue::as_dict)
        .expect("ws_upgrade");
    assert_eq!(
        upgrade.get("subprotocol").map(|v| v.display()).as_deref(),
        Some("v1.harn")
    );
    let headers = map
        .get("headers")
        .and_then(VmValue::as_dict)
        .expect("headers");
    assert_eq!(
        headers.get("Upgrade").map(|v| v.display()).as_deref(),
        Some("websocket")
    );
    assert_eq!(
        headers
            .get("Sec-WebSocket-Protocol")
            .map(|v| v.display())
            .as_deref(),
        Some("v1.harn")
    );
}

#[test]
fn http_upgrade_ws_picks_client_preferred_when_both_overlap() {
    // Regression for the divergence between
    // `http_upgrade_ws_impl`'s envelope-side negotiation and
    // `harn_serve::ws::negotiate_subprotocol`'s wire-side
    // negotiation. With client "v2.harn, v1.harn" and server
    // ["v1.harn", "v2.harn"] the two implementations used to
    // disagree (server-order picked v1; client-order picks v2).
    // The envelope MUST match what the upgrade handshake echoes
    // back, so we honour client preference everywhere.
    let req = VmValue::dict(crate::value::DictMap::from_iter([(
        crate::value::intern_key("headers"),
        VmValue::dict(crate::value::DictMap::from_iter([(
            crate::value::intern_key("Sec-WebSocket-Protocol"),
            VmValue::String(arcstr::ArcStr::from("v2.harn, v1.harn")),
        )])),
    )]));
    let options = VmValue::dict(crate::value::DictMap::from_iter([(
        crate::value::intern_key("subprotocols"),
        VmValue::List(std::sync::Arc::new(vec![
            VmValue::String(arcstr::ArcStr::from("v1.harn")),
            VmValue::String(arcstr::ArcStr::from("v2.harn")),
        ])),
    )]));
    let response = http_upgrade_ws_impl(&[req, options], &mut String::new()).unwrap();
    let upgrade = dict(&response)
        .get("ws_upgrade")
        .and_then(VmValue::as_dict)
        .expect("ws_upgrade");
    assert_eq!(
        upgrade.get("subprotocol").map(|v| v.display()).as_deref(),
        Some("v2.harn")
    );
}

#[test]
fn parse_envelope_round_trips_ws_upgrade_marker() {
    let req = VmValue::dict(crate::value::DictMap::from_iter([(
        crate::value::intern_key("headers"),
        VmValue::dict(crate::value::DictMap::from_iter([(
            crate::value::intern_key("Sec-WebSocket-Protocol"),
            VmValue::String(arcstr::ArcStr::from("v1.harn")),
        )])),
    )]));
    let options = VmValue::dict(crate::value::DictMap::from_iter([
        (
            crate::value::intern_key("subprotocols"),
            VmValue::List(std::sync::Arc::new(vec![VmValue::String(
                arcstr::ArcStr::from("v1.harn"),
            )])),
        ),
        (
            crate::value::intern_key("idle_ping_ms"),
            VmValue::Int(15_000),
        ),
    ]));
    let response = http_upgrade_ws_impl(&[req, options], &mut String::new()).unwrap();
    let json = vm_value_to_json(&response);
    let envelope = parse_envelope(&json).expect("envelope parses");
    let ws = envelope.ws_upgrade.expect("ws_upgrade present");
    assert_eq!(ws.subprotocol.as_deref(), Some("v1.harn"));
    assert_eq!(ws.offered, vec!["v1.harn"]);
    assert_eq!(ws.idle_ping_ms, Some(15_000));
    assert_eq!(envelope.status, 101);
}

#[test]
fn http_upgrade_ws_falls_through_when_no_subprotocols_offered() {
    let req = VmValue::dict(crate::value::DictMap::new());
    let response = http_upgrade_ws_impl(&[req], &mut String::new()).unwrap();
    let map = dict(&response);
    let upgrade = map
        .get("ws_upgrade")
        .and_then(VmValue::as_dict)
        .expect("ws_upgrade");
    assert!(matches!(upgrade.get("subprotocol"), Some(VmValue::Nil)));
}
