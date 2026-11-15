use super::*;

#[test]
fn only_stable_direct_http_calls_are_cancelled_by_disconnection() {
    for (protocol_version, expected) in [
        (rmcp::model::ProtocolVersion::V_2025_06_18, false),
        (rmcp::model::ProtocolVersion::V_2025_11_25, false),
        (rmcp::model::ProtocolVersion::V_2026_07_28, true),
    ] {
        let profile = McpRequestProfile { protocol_version };
        assert_eq!(profile.cancels_direct_call_on_disconnect(), expected);
    }
}

#[test]
fn protocol_registry_matches_official_sdk() {
    assert_eq!(
        PROTOCOL_VERSION,
        rmcp::model::ProtocolVersion::V_2026_07_28.as_str()
    );
    assert_eq!(
        sdk_protocol_versions(),
        rmcp::model::ProtocolVersion::KNOWN_VERSIONS
            .iter()
            .map(rmcp::model::ProtocolVersion::as_str)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        request_metadata_protocol_versions(),
        rmcp::model::ProtocolVersion::KNOWN_VERSIONS
            .iter()
            .filter(|version| *version >= &rmcp::model::ProtocolVersion::STANDARD_HEADERS)
            .map(rmcp::model::ProtocolVersion::as_str)
            .collect::<Vec<_>>()
    );
    assert_eq!(UNSUPPORTED_PROTOCOL_VERSION_CODE, -32022);
    assert_eq!(MISSING_REQUIRED_CLIENT_CAPABILITY_CODE, -32021);
    assert_eq!(HEADER_MISMATCH_CODE, -32020);
}

#[test]
fn initialize_negotiates_every_sdk_released_version() {
    for protocol_version in rmcp::model::ProtocolVersion::KNOWN_VERSIONS
        .iter()
        .filter(|version| *version < &rmcp::model::ProtocolVersion::STANDARD_HEADERS)
    {
        let outcome = negotiate_initialize(
            &json!({
                "protocolVersion": protocol_version.as_str(),
                "capabilities": {},
                "clientInfo": {"name": "codex-mcp-client", "version": "test"},
            }),
            json!({"tools": {}}),
            json!({"name": "harn", "version": "test"}),
            Some("test server"),
        )
        .expect("SDK-supported initialize version should negotiate");
        assert_eq!(&outcome.protocol_version, protocol_version);
        assert_eq!(outcome.client_identity, "codex-mcp-client/test");
        assert_eq!(
            outcome.result["protocolVersion"],
            json!(protocol_version.as_str())
        );
        assert_eq!(outcome.result["capabilities"]["tools"], json!({}));
        assert_eq!(outcome.result["serverInfo"]["name"], json!("harn"));
        assert_eq!(outcome.result["instructions"], json!("test server"));

        let typed: rmcp::model::InitializeResult = serde_json::from_value(outcome.result)
            .expect("initialize response must remain SDK-typed");
        assert_eq!(&typed.protocol_version, protocol_version);
    }

    let modern_initialize = negotiate_initialize(
        &json!({
            "protocolVersion": rmcp::model::ProtocolVersion::STANDARD_HEADERS.as_str(),
            "capabilities": {},
            "clientInfo": {"name": "codex-mcp-client", "version": "test"},
        }),
        json!({"tools": {}}),
        json!({"name": "harn", "version": "test"}),
        None,
    )
    .expect("modern initialize request should negotiate a released fallback");
    assert_eq!(
        modern_initialize.protocol_version,
        rmcp::model::ProtocolVersion::LATEST
    );
}

#[test]
fn initialized_session_keeps_its_version_when_request_meta_has_a_progress_token() {
    let mut session = McpServerSession::default();
    session
        .initialize(
            &json!({
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "codex-mcp-client", "version": "test"},
            }),
            json!({"tools": {}}),
            json!({"name": "harn", "version": "test"}),
            None,
        )
        .expect("initialize should negotiate");

    let profile = session
        .accept_request(
            &json!(2),
            "tools/call",
            &json!({"_meta": {"progressToken": "codex-proof"}}),
        )
        .expect("released request metadata should use the initialized version");
    assert!(!profile.uses_result_envelope());
}

#[test]
fn released_session_waits_for_initialized_notification_before_server_notifications() {
    let mut session = McpServerSession::default();
    session.accept_initialized_notification();
    assert!(!session.is_ready_for_notifications());
    session
        .initialize(
            &json!({
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "codex-mcp-client", "version": "test"},
            }),
            json!({"tools": {"listChanged": true}}),
            json!({"name": "harn", "version": "test"}),
            None,
        )
        .expect("initialize should negotiate");

    assert!(!session.is_ready_for_notifications());
    session.accept_initialized_notification();
    assert!(session.is_ready_for_notifications());
}

#[test]
fn completion_payload_dedupes_and_ranks_prefix_matches() {
    let response = completion_result(
        json!(1),
        vec![
            "typescript".to_string(),
            "rust".to_string(),
            "ruby".to_string(),
            "rust".to_string(),
        ],
        "ru",
    );
    assert_eq!(
        response["result"]["completion"]["values"],
        json!(["ruby", "rust"])
    );
    assert_eq!(response["result"]["completion"]["total"], json!(2));
    assert_eq!(response["result"]["completion"]["hasMore"], json!(false));
}

#[test]
fn task_capability_uses_the_stable_extension_map() {
    assert!(client_supports_tasks(&json!({
        "_meta": {
            MCP_META_KEY_CLIENT_CAPABILITIES: {
                "extensions": {TASKS_EXTENSION_ID: {}}
            }
        }
    })));
    assert!(!client_supports_tasks(&json!({})));
}

#[test]
fn application_metadata_keys_follow_mcp_grammar_and_reservations() {
    for valid in [
        "progressToken",
        "com.example/tool-version",
        "com.harnlang/toolContract",
    ] {
        validate_application_meta_key(valid).expect(valid);
    }
    for invalid in ["", "com.example/"] {
        assert_eq!(
            validate_application_meta_key(invalid),
            Err("has an empty metadata name".to_string()),
            "empty MCP metadata name accepted: {invalid:?}"
        );
    }
    for invalid in [
        "vendor.example/version/extra",
        "1example.vendor/version",
        "com..vendor/version",
        "com.mcp.tools/version",
        "io.modelcontextprotocol/private",
        "com.example/-version",
    ] {
        assert!(
            validate_application_meta_key(invalid).is_err(),
            "invalid MCP metadata key accepted: {invalid}"
        );
    }
}

#[test]
fn task_protocol_shapes_match_latest_spec_names() {
    assert_eq!(mcp_task_status_wire_name(McpTaskStatus::Working), "working");
    assert_eq!(
        mcp_task_status_wire_name(McpTaskStatus::InputRequired),
        "input_required"
    );
    assert!(McpTaskStatus::Completed.is_terminal());
    assert_eq!(tasks_capability()[TASKS_EXTENSION_ID], json!({}));
}

#[test]
fn mcp_list_page_uses_default_size_and_next_cursor() {
    let page = mcp_list_page(&json!({}), 105, "tools/list").unwrap();
    assert_eq!(page.start, 0);
    assert_eq!(page.end, DEFAULT_MCP_LIST_PAGE_SIZE);
    assert_eq!(
        page.next_cursor,
        Some(encode_mcp_list_cursor(DEFAULT_MCP_LIST_PAGE_SIZE))
    );

    let next = mcp_list_page(
        &json!({"cursor": page.next_cursor.unwrap()}),
        105,
        "tools/list",
    )
    .unwrap();
    assert_eq!(next.start, DEFAULT_MCP_LIST_PAGE_SIZE);
    assert_eq!(next.end, 105);
    assert_eq!(next.next_cursor, None);
}

#[test]
fn mcp_list_page_size_parses_positive_env_override() {
    assert_eq!(mcp_list_page_size_from_env(Some("2")), 2);
    assert_eq!(
        mcp_list_page_size_from_env(Some("0")),
        DEFAULT_MCP_LIST_PAGE_SIZE
    );
    assert_eq!(
        mcp_list_page_size_from_env(Some("nope")),
        DEFAULT_MCP_LIST_PAGE_SIZE
    );
    assert_eq!(
        mcp_list_page_size_from_env(None),
        DEFAULT_MCP_LIST_PAGE_SIZE
    );
}

#[test]
fn mcp_list_page_rejects_malformed_cursor() {
    let err = mcp_list_page(&json!({"cursor": "not-base64"}), 5, "resources/list")
        .expect_err("malformed cursor should fail");
    assert_eq!(err, "invalid resources/list cursor");
}

#[test]
fn list_response_pages_items_and_rejects_malformed_cursor() {
    let items = (0..DEFAULT_MCP_LIST_PAGE_SIZE + 5)
        .map(|index| json!({"name": format!("tool-{index}")}))
        .collect::<Vec<_>>();
    let first = list_response(json!(1), "tools/list", "tools", &json!({}), items.clone());
    let tools = first["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), DEFAULT_MCP_LIST_PAGE_SIZE);
    let cursor = first["result"]["nextCursor"].as_str().unwrap();

    let second = list_response(
        json!(2),
        "tools/list",
        "tools",
        &json!({"cursor": cursor}),
        items.clone(),
    );
    assert_eq!(second["result"]["tools"].as_array().unwrap().len(), 5);
    assert_eq!(
        second["result"]["tools"][0]["name"],
        json!(format!("tool-{DEFAULT_MCP_LIST_PAGE_SIZE}"))
    );
    assert!(second["result"].get("nextCursor").is_none());

    let malformed = list_response(
        json!(3),
        "tools/list",
        "tools",
        &json!({"cursor": "not-base64"}),
        items,
    );
    assert_eq!(malformed["error"]["code"], json!(-32602));
}

#[test]
fn response_envelope_carries_the_method_cache_hint() {
    let listed = apply_response_envelope(
        crate::jsonrpc::response(json!(1), json!({"tools": []})),
        "tools/list",
    );
    assert_eq!(listed["result"]["resultType"], RESULT_TYPE_COMPLETE);
    assert_eq!(listed["result"]["ttlMs"], json!(DEFAULT_LIST_CACHE_TTL_MS));
    let called = apply_response_envelope(
        crate::jsonrpc::response(json!(2), json!({"content": []})),
        "tools/call",
    );
    assert_eq!(called["result"]["resultType"], RESULT_TYPE_COMPLETE);
    assert!(called["result"].get("ttlMs").is_none());
    let failed = apply_response_envelope(
        crate::jsonrpc::error_response(json!(3), -32602, "bad"),
        "tools/list",
    );
    assert!(failed.get("result").is_none());
}

#[test]
fn stable_metadata_round_trips_through_meta_block() {
    let params = json!({
        "_meta": {
            MCP_META_KEY_PROTOCOL_VERSION: PROTOCOL_VERSION,
            MCP_META_KEY_CLIENT_INFO: {"name": "harn", "version": "x"},
            MCP_META_KEY_CLIENT_CAPABILITIES: {"roots": {}},
        }
    });
    let meta = parse_request_metadata(&params);
    assert_eq!(
        meta.protocol_version()
            .as_ref()
            .map(|version| version.as_str()),
        Some(PROTOCOL_VERSION)
    );
    assert_eq!(
        serde_json::to_value(meta.client_info()).unwrap(),
        json!({"name": "harn", "version": "x"})
    );
    assert_eq!(
        serde_json::to_value(meta.client_capabilities()).unwrap(),
        json!({"roots": {}})
    );
    enforce_request_protocol_version(&json!(1), &meta).unwrap();
}

#[test]
fn stable_metadata_is_required() {
    let meta = parse_request_metadata(&json!({}));
    assert_eq!(meta, McpRequestMetadata::default());
    let error = enforce_request_protocol_version(&json!(1), &meta).unwrap_err();
    assert_eq!(error["error"]["code"], json!(-32602));
}

#[test]
fn enforce_request_protocol_version_rejects_unknown_version() {
    let meta = parse_request_metadata(&json!({
        "_meta": {MCP_META_KEY_PROTOCOL_VERSION: "2099-01-01"}
    }));
    let id = json!(7);
    let err =
        enforce_request_protocol_version(&id, &meta).expect_err("unknown version should error");
    assert_eq!(err["id"], id);
    assert_eq!(
        err["error"]["code"],
        json!(UNSUPPORTED_PROTOCOL_VERSION_CODE)
    );
    assert_eq!(err["error"]["data"]["requested"], json!("2099-01-01"));
    let supported = err["error"]["data"]["supported"].as_array().unwrap();
    assert!(supported.contains(&json!(PROTOCOL_VERSION)));
}

#[test]
fn enforce_request_protocol_version_accepts_stable_metadata() {
    let meta = parse_request_metadata(&json!({
        "_meta": {
            MCP_META_KEY_PROTOCOL_VERSION: PROTOCOL_VERSION,
            MCP_META_KEY_CLIENT_INFO: {"name": "harn", "version": "x"},
            MCP_META_KEY_CLIENT_CAPABILITIES: {},
        }
    }));
    enforce_request_protocol_version(&json!(1), &meta).unwrap();
}

#[test]
fn enforce_request_protocol_version_uses_sdk_required_metadata_validation() {
    let meta = parse_request_metadata(&json!({
        "_meta": {MCP_META_KEY_PROTOCOL_VERSION: PROTOCOL_VERSION}
    }));
    let error = enforce_request_protocol_version(&json!(1), &meta)
        .expect_err("stable requests require typed client capabilities");
    assert_eq!(error["error"]["code"], json!(-32602));
    assert!(error["error"]["message"]
        .as_str()
        .is_some_and(|message| message.contains(MCP_META_KEY_CLIENT_CAPABILITIES)));
}

#[test]
fn enforce_request_protocol_version_rejects_old_versions() {
    let meta = parse_request_metadata(&json!({
        "_meta": {
            MCP_META_KEY_PROTOCOL_VERSION: "2025-06-18",
            MCP_META_KEY_CLIENT_INFO: {"name": "old", "version": "1"},
            MCP_META_KEY_CLIENT_CAPABILITIES: {},
        }
    }));
    let error = enforce_request_protocol_version(&json!(1), &meta).unwrap_err();
    assert_eq!(
        error["error"]["code"],
        json!(UNSUPPORTED_PROTOCOL_VERSION_CODE)
    );
}

#[test]
fn negotiate_standard_http_headers_detects_stable_protocol_header() {
    let headers = std::collections::HashMap::from([
        (
            MCP_HEADER_PROTOCOL_VERSION.to_string(),
            PROTOCOL_VERSION.to_string(),
        ),
        (MCP_HEADER_METHOD.to_string(), "tools/list".to_string()),
    ]);
    let outcome = negotiate_http_request(
        |key| headers.get(key).map(String::as_str),
        Some("tools/list"),
        None,
        &json!(1),
    )
    .unwrap();
    assert_eq!(outcome.protocol_version.as_deref(), Some(PROTOCOL_VERSION));
}

#[test]
fn negotiate_standard_http_headers_rejects_method_body_mismatch() {
    let headers = std::collections::HashMap::from([
        (
            MCP_HEADER_PROTOCOL_VERSION.to_string(),
            PROTOCOL_VERSION.to_string(),
        ),
        (MCP_HEADER_METHOD.to_string(), "tools/list".to_string()),
    ]);
    let err = negotiate_http_request(
        |key| headers.get(key).map(String::as_str),
        Some("tools/call"),
        None,
        &json!(2),
    )
    .expect_err("header/body mismatch must error");
    assert_eq!(err["error"]["code"], json!(HEADER_MISMATCH_CODE));
    assert_eq!(err["error"]["data"]["headerValue"], json!("tools/list"));
    assert_eq!(err["error"]["data"]["bodyMethod"], json!("tools/call"));
}

#[test]
fn negotiate_standard_http_headers_rejects_name_body_mismatch() {
    let headers = std::collections::HashMap::from([
        (
            MCP_HEADER_PROTOCOL_VERSION.to_string(),
            PROTOCOL_VERSION.to_string(),
        ),
        (MCP_HEADER_METHOD.to_string(), "tools/call".to_string()),
        (MCP_HEADER_NAME.to_string(), "wrong".to_string()),
    ]);
    let err = negotiate_http_request(
        |key| headers.get(key).map(String::as_str),
        Some("tools/call"),
        Some("right"),
        &json!(3),
    )
    .expect_err("name mismatch must error");
    assert_eq!(err["error"]["code"], json!(HEADER_MISMATCH_CODE));
    assert_eq!(err["error"]["data"]["bodyName"], json!("right"));
}

#[test]
fn negotiate_standard_http_headers_rejects_missing_required_headers() {
    let none = std::collections::HashMap::<String, String>::new();
    let missing_protocol = negotiate_http_request(
        |key| none.get(key).map(String::as_str),
        Some("tools/list"),
        None,
        &json!(4),
    )
    .expect_err("modern requests require a protocol header");
    assert_eq!(
        missing_protocol["error"]["code"],
        json!(HEADER_MISMATCH_CODE)
    );

    let headers = std::collections::HashMap::from([
        (
            MCP_HEADER_PROTOCOL_VERSION.to_string(),
            PROTOCOL_VERSION.to_string(),
        ),
        (MCP_HEADER_METHOD.to_string(), "tools/call".to_string()),
    ]);
    let missing_name = negotiate_http_request(
        |key| headers.get(key).map(String::as_str),
        Some("tools/call"),
        Some("weather"),
        &json!(5),
    )
    .expect_err("named requests require Mcp-Name");
    assert_eq!(missing_name["error"]["code"], json!(HEADER_MISMATCH_CODE));
}

#[test]
fn protocol_validation_errors_require_http_bad_request() {
    for code in [
        -32602,
        HEADER_MISMATCH_CODE,
        MISSING_REQUIRED_CLIENT_CAPABILITY_CODE,
        UNSUPPORTED_PROTOCOL_VERSION_CODE,
    ] {
        assert!(requires_http_bad_request(&json!({"error": {"code": code}})));
    }
    assert!(!requires_http_bad_request(
        &json!({"error": {"code": -32601}})
    ));
    assert!(!requires_http_bad_request(&json!({"result": {}})));
}

#[test]
fn stable_request_identity_never_leaks_from_a_previous_request() {
    let mut session = McpServerSession::default();
    let with_identity = json!({
        "_meta": {
            MCP_META_KEY_PROTOCOL_VERSION: PROTOCOL_VERSION,
            MCP_META_KEY_CLIENT_INFO: {"name": "first", "version": "1"},
            MCP_META_KEY_CLIENT_CAPABILITIES: {},
        }
    });
    session
        .accept_request(&json!(1), METHOD_SERVER_DISCOVER, &with_identity)
        .unwrap();
    assert_eq!(session.client_identity(), "first/1");

    let without_identity = json!({
        "_meta": {
            MCP_META_KEY_PROTOCOL_VERSION: PROTOCOL_VERSION,
            MCP_META_KEY_CLIENT_CAPABILITIES: {},
        }
    });
    session
        .accept_request(&json!(2), METHOD_SERVER_DISCOVER, &without_identity)
        .unwrap();
    assert_eq!(session.client_identity(), "unknown");
}

#[test]
fn standard_name_header_value_extracts_method_subject() {
    assert_eq!(
        standard_name_header_value("tools/call", &json!({"name": "demo"})),
        Some("demo".to_string())
    );
    assert_eq!(
        standard_name_header_value("prompts/get", &json!({"name": "p"})),
        Some("p".to_string())
    );
    assert_eq!(
        standard_name_header_value("resources/read", &json!({"uri": "harn://x"})),
        Some("harn://x".to_string())
    );
    for method in [METHOD_TASKS_GET, METHOD_TASKS_UPDATE, METHOD_TASKS_CANCEL] {
        assert_eq!(
            standard_name_header_value(method, &json!({"taskId": "task-123"})),
            Some("task-123".to_string())
        );
    }
    assert_eq!(standard_name_header_value("tools/list", &json!({})), None);
}

#[test]
fn routing_headers_decode_values_before_comparing_request_identity() {
    let examples = header_value_examples();
    let examples = examples.as_array().unwrap();
    assert_eq!(examples.len(), 7);
    for example in examples {
        let value = example["value"].as_str().unwrap();
        if value.is_empty() {
            continue;
        }
        let params = json!({"name": value});
        let encoded = standard_name_header_value(METHOD_TOOLS_CALL, &params).unwrap();
        let body = standard_name_body_value(METHOD_TOOLS_CALL, &params).unwrap();
        assert_eq!(body, value);
        let headers = std::collections::HashMap::from([
            (MCP_HEADER_PROTOCOL_VERSION, PROTOCOL_VERSION),
            (MCP_HEADER_METHOD, METHOD_TOOLS_CALL),
            (MCP_HEADER_NAME, encoded.as_str()),
        ]);
        negotiate_http_request(
            |key| headers.get(key).copied(),
            Some(METHOD_TOOLS_CALL),
            Some(&body),
            &json!(1),
        )
        .unwrap();
        let mismatch = negotiate_http_request(
            |key| headers.get(key).copied(),
            Some(METHOD_TOOLS_CALL),
            Some("different"),
            &json!(1),
        )
        .unwrap_err();
        assert_eq!(mismatch["error"]["code"], HEADER_MISMATCH_CODE);
    }
    let malformed = negotiate_http_request(
        |key| match key {
            MCP_HEADER_PROTOCOL_VERSION => Some(PROTOCOL_VERSION),
            MCP_HEADER_METHOD => Some(METHOD_TOOLS_CALL),
            MCP_HEADER_NAME => Some("=?base64?/w==?="),
            _ => None,
        },
        Some(METHOD_TOOLS_CALL),
        Some("Δοκιμή"),
        &json!(1),
    )
    .unwrap_err();
    assert_eq!(malformed["error"]["code"], HEADER_MISMATCH_CODE);
}

#[test]
fn missing_task_capability_uses_the_extension_error_contract() {
    let response = missing_tasks_capability_response(json!(7));
    assert_eq!(response["error"]["code"], -32021);
    assert_eq!(
        response["error"]["data"]["requiredCapabilities"]["extensions"][TASKS_EXTENSION_ID],
        json!({})
    );
}

#[test]
fn apply_result_envelope_adds_result_type_and_cache() {
    let mut stable = json!({"tools": []});
    apply_result_envelope(&mut stable, Some(&McpCacheHint::list_default()));
    assert_eq!(stable["resultType"], json!(RESULT_TYPE_COMPLETE));
    assert_eq!(stable["ttlMs"], json!(DEFAULT_LIST_CACHE_TTL_MS));
    assert_eq!(stable["cacheScope"], json!(DEFAULT_LIST_CACHE_SCOPE));
}

#[test]
fn apply_result_envelope_preserves_caller_provided_result_type() {
    let mut result = json!({"resultType": RESULT_TYPE_INPUT_REQUIRED});
    apply_result_envelope(&mut result, None);
    assert_eq!(result["resultType"], json!(RESULT_TYPE_INPUT_REQUIRED));
}

#[test]
fn server_discover_result_advertises_request_metadata_versions() {
    let discover = server_discover_result(
        json!({"tools": {}}),
        json!({"name": "harn", "version": "x"}),
        Some("hello"),
    );
    assert_eq!(discover["resultType"], json!(RESULT_TYPE_COMPLETE));
    assert_eq!(discover["ttlMs"], json!(0));
    assert_eq!(discover["cacheScope"], json!("private"));
    assert_eq!(
        discover["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
        json!("harn")
    );
    let supported = discover["supportedVersions"].as_array().unwrap();
    assert_eq!(supported, &[json!(PROTOCOL_VERSION)]);
    assert_eq!(discover["instructions"], json!("hello"));

    let typed: rmcp::model::DiscoverResult = serde_json::from_value(discover)
        .expect("Harn discovery result must match the official SDK type");
    assert_eq!(typed.result_type, rmcp::model::ResultType::COMPLETE);
    assert_eq!(typed.ttl_ms, 0);
    assert_eq!(
        typed.server_info().map(|info| info.name),
        Some("harn".to_string())
    );
}

#[test]
fn subscriptions_listen_is_explicitly_unsupported() {
    let response = explicit_unsupported_method_response(json!(7), "subscriptions/listen")
        .expect("known unsupported stable method");
    assert_eq!(response["error"]["code"], json!(-32601));
    assert_eq!(
        response["error"]["data"]["type"],
        json!("mcp.unsupportedFeature")
    );
    assert_eq!(
        response["error"]["data"]["protocolVersion"],
        json!(PROTOCOL_VERSION)
    );
}

#[test]
fn stable_input_methods_are_rejected_as_top_level_server_calls() {
    for method in [METHOD_SAMPLING_CREATE_MESSAGE, METHOD_ELICITATION_CREATE] {
        let response = explicit_unsupported_method_response(json!(7), method)
            .expect("input method has an explicit boundary error");
        assert_eq!(response["error"]["code"], json!(-32601));
        assert_eq!(response["error"]["data"]["method"], json!(method));
    }
}
