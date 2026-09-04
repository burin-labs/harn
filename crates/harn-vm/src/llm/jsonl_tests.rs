use super::*;

#[test]
fn roundtrip_preserves_text_and_tool_calls() {
    let mock = parse_llm_mock_value(&serde_json::json!({
        "text": "hello",
        "model": "mock",
        "simulated_cost_usd": 0.0025,
        "tool_calls": [
            { "name": "search", "args": { "q": "harn" } }
        ]
    }))
    .expect("parse");
    let line = serialize_llm_mock(mock).expect("serialize");
    let value: serde_json::Value = serde_json::from_str(&line).expect("reparse");
    let reparsed = parse_llm_mock_value(&value).expect("reparse mock");
    assert_eq!(reparsed.text, "hello");
    assert_eq!(reparsed.tool_calls.len(), 1);
    assert_eq!(reparsed.tool_calls[0]["name"].as_str(), Some("search"));
    assert_eq!(reparsed.simulated_cost_usd, Some(0.0025));
}

#[test]
fn simulated_cost_must_be_finite_and_nonnegative() {
    for value in [serde_json::json!(-0.1), serde_json::json!("free")] {
        let error = parse_llm_mock_value(&serde_json::json!({
            "text": "hello",
            "simulated_cost_usd": value,
        }))
        .expect_err("invalid simulated cost");
        assert!(error.contains("finite non-negative number"), "{error}");
    }
}

#[test]
fn roundtrip_preserves_raw_tool_calls() {
    let mock = parse_llm_mock_value(&serde_json::json!({
        "text": "hello",
        "model": "mock",
        "raw_tool_calls": [
            {
                "id": "call_1",
                "type": "function",
                "function": {
                    "name": "tool_call",
                    "arguments": "{\"cmd\":\"test\"}"
                }
            }
        ]
    }))
    .expect("parse");
    let line = serialize_llm_mock(mock).expect("serialize");
    let value: serde_json::Value = serde_json::from_str(&line).expect("reparse json");
    let reparsed = parse_llm_mock_value(&value).expect("reparse mock");
    assert_eq!(
        reparsed.raw_tool_calls[0]["function"]["name"].as_str(),
        Some("tool_call")
    );
    assert_eq!(
        reparsed.raw_tool_calls[0]["function"]["arguments"].as_str(),
        Some("{\"cmd\":\"test\"}")
    );
}

#[test]
fn parse_does_not_synthesize_raw_tool_calls_from_normalized_calls() {
    let mock = parse_llm_mock_value(&serde_json::json!({
        "text": "hello",
        "model": "mock",
        "tool_calls": [
            {
                "name": "search",
                "arguments": {"query": "rust"}
            }
        ]
    }))
    .expect("parse");

    assert!(
        mock.raw_tool_calls.is_empty(),
        "normalized tool_calls must not be promoted to provider-native raw_tool_calls"
    );
}

#[test]
fn v0_tool_call_normalization_preserves_provider_metadata() {
    let mock = parse_llm_mock_value(&serde_json::json!({
        "tool_calls": [{
            "id": "gemini_call_1",
            "type": "function",
            "name": "lookup",
            "args": {"query": "harn"},
            "thought_signature": "opaque-gemini-signature"
        }]
    }))
    .expect("parse legacy provider metadata");

    let call = &mock.tool_calls[0];
    assert_eq!(call["id"], "gemini_call_1");
    assert_eq!(call["type"], "function");
    assert_eq!(call["name"], "lookup");
    assert_eq!(call["arguments"], serde_json::json!({"query": "harn"}));
    assert_eq!(call["thought_signature"], "opaque-gemini-signature");
    assert!(call.get("args").is_none(), "legacy alias is canonicalized");
}

/// harn#7860 falsifier. A scripted zero-token completion must stay zero.
/// Before this, `usage` was accepted by the parser and dropped, so the
/// entry fell through to the thirty-token default and the response the
/// author wrote as empty read as billed work.
#[test]
fn a_scripted_usage_sets_the_token_counts_it_names() {
    let entry = parse_llm_mock_value(&serde_json::json!({
        "text": "",
        "usage": {"completion_tokens": 0},
    }))
    .expect("a usage-only entry parses");
    assert_eq!(
        entry.output_tokens,
        Some(0),
        "a scripted zero must survive as a zero, not fall through to the default"
    );

    // Both provider spellings, and the flat field still wins over `usage`
    // so an entry carrying both is not ambiguous.
    let openai = parse_llm_mock_value(&serde_json::json!({
        "usage": {"prompt_tokens": 7, "completion_tokens": 11},
    }))
    .expect("openai-compat spellings parse");
    assert_eq!(
        (openai.input_tokens, openai.output_tokens),
        (Some(7), Some(11))
    );

    let both = parse_llm_mock_value(&serde_json::json!({
        "output_tokens": 5,
        "usage": {"completion_tokens": 11},
    }))
    .expect("an entry carrying both parses");
    assert_eq!(
        both.output_tokens,
        Some(5),
        "the flat field is authoritative"
    );

    // Negative control: a misspelled key inside `usage` fails loudly rather
    // than repeating the silent drop this test exists to close.
    let error = parse_llm_mock_value(&serde_json::json!({
        "usage": {"completion_tokins": 0},
    }))
    .expect_err("a misspelled usage field must not be dropped");
    assert!(
        error.contains("unknown usage field `completion_tokins`"),
        "{error}"
    );

    // An entry with no `usage` at all is unchanged.
    let bare =
        parse_llm_mock_value(&serde_json::json!({"text": "hi"})).expect("a bare entry parses");
    assert_eq!((bare.input_tokens, bare.output_tokens), (None, None));
}

/// `total_tokens` is real provider vocabulary that existing fixtures write,
/// but the mock derives the total and has no slot for it. Accepting it and
/// dropping it would repeat the defect this parser change closes, so it is
/// honoured as a constraint on the scripted split.
#[test]
fn a_scripted_total_is_checked_against_the_split_it_names() {
    let agreed = parse_llm_mock_value(&serde_json::json!({
        "usage": {"input_tokens": 11, "output_tokens": 7, "total_tokens": 18},
    }))
    .expect("a total agreeing with the split parses");
    assert_eq!(
        (agreed.input_tokens, agreed.output_tokens),
        (Some(11), Some(7))
    );

    let contradiction = parse_llm_mock_value(&serde_json::json!({
        "usage": {"input_tokens": 11, "output_tokens": 7, "total_tokens": 99},
    }))
    .expect_err("a total contradicting the split must not be dropped");
    assert!(contradiction.contains("total_tokens"), "{contradiction}");

    // A total with nothing to check it against is rejected rather than
    // silently ignored: the mock cannot split one number into two.
    let unsplittable = parse_llm_mock_value(&serde_json::json!({
        "usage": {"total_tokens": 18},
    }))
    .expect_err("an uncheckable total must fail at the fixture that wrote it");
    assert!(unsplittable.contains("total_tokens"), "{unsplittable}");
}

#[test]
fn parse_rejects_unknown_error_category() {
    let result = parse_llm_mock_value(&serde_json::json!({
        "error": { "category": "wibble", "message": "x" }
    }));
    match result {
        Err(err) => assert!(err.contains("unknown error category"), "{err}"),
        Ok(_) => panic!("expected parse failure for unknown error category"),
    }
}

#[test]
fn parses_explicit_generic_error_category() {
    let mock = parse_llm_mock_value(&serde_json::json!({
        "error": { "category": "generic", "message": "x" }
    }))
    .expect("parse generic error");
    let error = mock.error.expect("error");
    assert_eq!(error.category.as_str(), "generic");
    assert_eq!(error.message, "x");
}

#[test]
fn parses_provider_error_envelope() {
    let mock = parse_llm_mock_value(&serde_json::json!({
        "error": {
            "status": 503,
            "kind": "transient",
            "reason": "upstream_unavailable",
            "message": "upstream unavailable",
            "retry_after_ms": 250
        }
    }))
    .expect("parse provider envelope");
    let error = mock.error.expect("error");
    assert_eq!(error.category.as_str(), "overloaded");
    assert_eq!(error.status, Some(503));
    assert_eq!(error.kind.as_deref(), Some("transient"));
    assert_eq!(error.reason.as_deref(), Some("upstream_unavailable"));
    assert_eq!(error.retry_after_ms, Some(250));
}

#[test]
fn roundtrip_preserves_provider_error_envelope() {
    let mock = parse_llm_mock_value(&serde_json::json!({
        "match": "*retry*",
        "error": {
            "status": 503,
            "kind": "transient",
            "reason": "upstream_unavailable",
            "retry_after_ms": 250
        }
    }))
    .expect("parse provider envelope");
    let line = serialize_llm_mock(mock).expect("serialize");
    let value: serde_json::Value = serde_json::from_str(&line).expect("reparse json");
    let reparsed = parse_llm_mock_value(&value).expect("reparse mock");
    let error = reparsed.error.expect("error");
    assert_eq!(reparsed.match_pattern.as_deref(), Some("*retry*"));
    assert_eq!(error.category.as_str(), "overloaded");
    assert_eq!(error.status, Some(503));
    assert_eq!(error.kind.as_deref(), Some("transient"));
    assert_eq!(error.reason.as_deref(), Some("upstream_unavailable"));
    assert_eq!(error.retry_after_ms, Some(250));
}

#[test]
fn parse_rejects_unknown_error_kind() {
    let result = parse_llm_mock_value(&serde_json::json!({
        "error": { "status": 503, "kind": "maybe" }
    }));
    match result {
        Err(err) => assert!(err.contains("unknown error kind"), "{err}"),
        Ok(_) => panic!("expected parse failure for unknown error kind"),
    }
}

// --- Versioned mock-fixture contract (bc#4969) ---

#[test]
fn header_detects_v0_when_no_schema_version() {
    assert_eq!(
        parse_fixture_header(&serde_json::json!({"text": "hi"})).expect("header"),
        None,
        "an ordinary entry is not a header"
    );
}

#[test]
fn header_reads_version_and_strict_scopes() {
    assert_eq!(
        parse_fixture_header(&serde_json::json!({"schemaVersion": 1, "strictScopes": true}))
            .expect("header"),
        Some((1, true))
    );
}

#[test]
fn v1_header_is_closed_and_requires_strict_scopes() {
    let missing = parse_fixture_header(&serde_json::json!({"schemaVersion": 1}))
        .expect_err("v1 header must declare strictScopes");
    assert!(missing.contains("strictScopes"), "{missing}");

    let unknown = parse_fixture_header(&serde_json::json!({
        "schemaVersion": 1,
        "strictScopes": false,
        "strictScope": false,
    }))
    .expect_err("v1 header must reject misspelled fields");
    assert!(unknown.contains("unknown field"), "{unknown}");
}

#[test]
fn header_rejects_unsupported_schema_version() {
    let err = parse_fixture_header(&serde_json::json!({"schemaVersion": 2, "strictScopes": false}))
        .expect_err("unsupported version must fail");
    assert!(err.contains("unsupported schemaVersion"), "{err}");
}

#[test]
fn v0_parse_pins_default_scope_and_legacy_consume() {
    // FIFO entry: always consumed (not sticky).
    let fifo = parse_llm_mock_value(&serde_json::json!({"text": "x"})).expect("fifo");
    assert_eq!(fifo.scope, DEFAULT_MOCK_SCOPE);
    assert!(!fifo.sticky);
    // Glob entry: reusable (sticky) unless consume_match.
    let glob = parse_llm_mock_value(&serde_json::json!({"match": "*"})).expect("glob");
    assert!(glob.sticky, "v0 reusable glob is sticky");
    let glob_once = parse_llm_mock_value(&serde_json::json!({"match": "*", "consume_match": true}))
        .expect("glob once");
    assert!(!glob_once.sticky, "consume_match makes a glob one-shot");
}

/// The issue's falsifier: an inline-enqueued entry carrying a key the
/// parser does not read must fail with that key named. It used to succeed
/// and drop the value, which makes a fixture that was never applied
/// indistinguishable from one that was.
#[test]
fn v0_rejects_an_unknown_key_and_names_the_field_meant() {
    let error = parse_llm_mock_value(&serde_json::json!({
        "text": "done",
        "outupt_tokens": 5,
    }))
    .expect_err("a key the parser never reads must not pass silently");
    assert!(
        error.contains("`outupt_tokens`"),
        "the message names the offending key: {error}"
    );
    assert!(
        error.contains("`output_tokens`"),
        "the message names the field the author probably meant: {error}"
    );
}

/// A v1 field in a v0 document is not a misspelling, so a nearest-spelling
/// guess would be noise. Name the missing header instead.
#[test]
fn v0_rejects_a_v1_only_key_by_naming_the_missing_header() {
    for field in ["id", "scope", "consume"] {
        let mut entry = serde_json::Map::new();
        entry.insert("text".to_string(), serde_json::json!("done"));
        entry.insert(field.to_string(), serde_json::json!("main"));
        let error = parse_llm_mock_value(&serde_json::Value::Object(entry))
            .expect_err("a v1 field is dropped at v0, so it must not parse");
        assert!(error.contains(field), "{error}");
        assert!(
            error.contains("schemaVersion"),
            "the message points at the missing header: {error}"
        );
    }
}

/// The control that keeps the check from passing by rejecting everything.
/// Every field the v0 parser reads must still round-trip, including the
/// two spellings the versioned contract dropped.
#[test]
fn v0_still_accepts_every_field_it_reads() {
    let entry = parse_llm_mock_value(&serde_json::json!({
        "match": "*",
        "consume_match": true,
        "text": "done",
        "usage": {"input_tokens": 1},
        "input_tokens": 2,
        "output_tokens": 3,
        "cache_read_tokens": 4,
        "cache_write_tokens": 5,
        "cache_creation_input_tokens": 6,
        "simulated_cost_usd": 0.5,
        "thinking": "t",
        "thinking_summary": "ts",
        "stop_reason": "end_turn",
        "model": "m",
        "provider": "p",
        "blocks": [],
        "logprobs": [],
        "tool_calls": [{"name": "look", "args": {}}],
        "raw_tool_calls": [],
        "error": null,
        "stream_chunks": ["a"],
    }))
    .expect("every v0 field still parses");
    assert_eq!(entry.text, "done");
    assert_eq!(entry.input_tokens, Some(2));
}

/// The suggestion is shared with the versioned contract, so a v1 typo
/// gets the same help rather than a bare rejection.
#[test]
fn v1_unknown_field_also_names_the_field_meant() {
    let error = parse_llm_mock_value_versioned(
        &serde_json::json!({
            "id": "main-1",
            "scope": "main",
            "consume": "once",
            "stpo_reason": "end_turn",
        }),
        1,
        0,
    )
    .expect_err("v1 is closed");
    assert!(error.contains("`stpo_reason`"), "{error}");
    assert!(error.contains("`stop_reason`"), "{error}");
}

#[test]
fn v1_parse_reads_scope_consume_and_id() {
    let entry = parse_llm_mock_value_versioned(
        &serde_json::json!({"scope": "completion.judge", "consume": "sticky", "id": "j1", "text": "Y"}),
        1,
        7,
    )
    .expect("v1 entry");
    assert_eq!(entry.scope, "completion.judge");
    assert!(entry.sticky);
    assert_eq!(entry.entry_id, "j1");
}

#[test]
fn v1_rejects_missing_metadata_and_legacy_aliases() {
    for (field, value) in [
        ("id", serde_json::json!({"scope":"main","consume":"once"})),
        ("scope", serde_json::json!({"id":"main-1","consume":"once"})),
        ("consume", serde_json::json!({"id":"main-1","scope":"main"})),
    ] {
        let error =
            parse_llm_mock_value_versioned(&value, 1, 0).expect_err("v1 metadata must be explicit");
        assert!(error.contains(field), "{error}");
    }

    let error = parse_llm_mock_value_versioned(
        &serde_json::json!({
            "id":"main-1",
            "scope":"main",
            "consume":"once",
            "tool_calls":[{"name":"look","args":{}}]
        }),
        1,
        0,
    )
    .expect_err("legacy args alias is v0-only");
    assert!(
        error.contains("unknown tool_calls[0] field `args`"),
        "{error}"
    );

    let error = parse_llm_mock_value_versioned(
        &serde_json::json!({
            "id":"main-1",
            "scope":"main",
            "consume":"once",
            "tool_calls":[{
                "name":"look",
                "arguments":{},
                "thought_signature":"opaque-gemini-signature"
            }]
        }),
        1,
        0,
    )
    .expect_err("provider-specific metadata is v0-only until the v1 contract owns it");
    assert!(
        error.contains("unknown tool_calls[0] field `thought_signature`"),
        "{error}"
    );

    let entry = parse_llm_mock_value_versioned(
        &serde_json::json!({
            "id":"main-1",
            "scope":"main",
            "consume":"once",
            "tool_calls":[{
                "id":"gemini_call_1",
                "type":"function",
                "name":"look",
                "arguments":{},
                "provider_metadata": {
                    "gemini": {"thought_signature":"opaque-gemini-signature"}
                }
            }]
        }),
        1,
        0,
    )
    .expect("v1 accepts namespaced provider metadata");
    assert_eq!(entry.tool_calls[0]["id"], "gemini_call_1");
    assert_eq!(
        entry.tool_calls[0]["provider_metadata"]["gemini"]["thought_signature"],
        "opaque-gemini-signature"
    );

    let error = parse_llm_mock_value_versioned(
        &serde_json::json!({
            "id":"main-1",
            "scope":"main",
            "consume":"once",
            "text":"ok",
            "texte":"typo"
        }),
        1,
        0,
    )
    .expect_err("v1 fixture entries are closed");
    assert!(
        error.contains("unknown fixture entry field `texte`"),
        "{error}"
    );
}

#[test]
fn v1_parse_rejects_unknown_consume_mode() {
    let err = parse_llm_mock_value_versioned(
        &serde_json::json!({"id":"main-1", "scope":"main", "consume": "maybe"}),
        1,
        0,
    )
    .expect_err("unknown consume must fail");
    assert!(err.contains("consume"), "{err}");
}

#[test]
fn v1_serializer_emits_a_closed_document_that_reparses() {
    let mock = parse_llm_mock_value(&serde_json::json!({
        "text": "hello",
        "tool_calls": [{
            "id": "gemini_call_1",
            "type": "function",
            "name": "look",
            "args": {"file": "src/lib.harn"},
            "thought_signature": "opaque-gemini-signature"
        }],
    }))
    .expect("parse legacy source entry");

    let document = serialize_llm_mock_fixture(vec![mock]).expect("serialize v1 document");
    let serialized_entry: serde_json::Value = serde_json::from_str(
        document
            .lines()
            .nth(1)
            .expect("fixture contains one serialized entry"),
    )
    .expect("serialized entry is JSON");
    assert_eq!(serialized_entry["tool_calls"][0]["id"], "gemini_call_1");
    assert_eq!(serialized_entry["tool_calls"][0]["type"], "function");
    assert_eq!(
        serialized_entry["tool_calls"][0]["provider_metadata"]["gemini"]["thought_signature"],
        "opaque-gemini-signature"
    );
    let fixture = parse_llm_mocks_jsonl(&document).expect("reparse v1 document");
    assert_eq!(fixture.schema_version, 1);
    assert!(!fixture.strict_scopes);
    assert_eq!(fixture.mocks.len(), 1);
    assert_eq!(fixture.mocks[0].entry_id, "record-0");
    assert_eq!(fixture.mocks[0].scope, DEFAULT_MOCK_SCOPE);
    assert!(!fixture.mocks[0].sticky);
    assert_eq!(
        fixture.mocks[0].tool_calls[0]["arguments"]["file"].as_str(),
        Some("src/lib.harn")
    );
    assert_eq!(fixture.mocks[0].tool_calls[0]["id"], "gemini_call_1");
    assert_eq!(
        fixture.mocks[0].tool_calls[0]["provider_metadata"]["gemini"]["thought_signature"],
        "opaque-gemini-signature"
    );
}

#[test]
fn load_rejects_unsupported_schema_version() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("fixture.jsonl");
    std::fs::write(
        &path,
        "{\"schemaVersion\": 2, \"strictScopes\": false}\n{\"scope\": \"main\", \"text\": \"MAIN\"}\n",
    )
    .expect("write fixture");
    let err = load_llm_mocks_jsonl(&path).expect_err("unsupported version must fail at load");
    assert!(err.contains("unsupported schemaVersion"), "{err}");
}

#[test]
fn load_parses_v1_header_and_scoped_entries() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("fixture.jsonl");
    std::fs::write(
        &path,
        "{\"schemaVersion\": 1, \"strictScopes\": true}\n\
         {\"id\": \"main-1\", \"scope\": \"agent.main\", \"consume\": \"once\", \"text\": \"MAIN\"}\n\
         {\"id\": \"judge-1\", \"scope\": \"completion.judge\", \"consume\": \"sticky\", \"match\": \"*\", \"text\": \"JUDGE\"}\n",
    )
    .expect("write fixture");
    let fixture = load_llm_mocks_jsonl(&path).expect("load v1");
    assert_eq!(fixture.schema_version, 1);
    assert!(fixture.strict_scopes);
    assert_eq!(fixture.mocks.len(), 2);
    assert_eq!(fixture.mocks[0].scope, "agent.main");
    assert_eq!(fixture.mocks[0].entry_id, "main-1");
    assert!(!fixture.mocks[0].sticky);
    assert_eq!(fixture.mocks[1].scope, "completion.judge");
    assert_eq!(fixture.mocks[1].entry_id, "judge-1");
    assert!(fixture.mocks[1].sticky);
    assert!(fixture.warnings.is_empty());
}

#[test]
fn v1_host_namespaced_scopes_are_open_without_advisory_noise() {
    let fixture = parse_llm_mocks_jsonl(
        "{\"schemaVersion\":1,\"strictScopes\":false}\n\
         {\"id\":\"one\",\"scope\":\"custom.review\",\"consume\":\"once\",\"text\":\"ONE\"}\n\
         {\"id\":\"two\",\"scope\":\"custom.review\",\"consume\":\"once\",\"text\":\"TWO\"}\n",
    )
    .expect("open scope strings remain valid");
    assert!(fixture.warnings.is_empty());
}

#[test]
fn v1_unqualified_unknown_scopes_are_advisory_and_deduplicated() {
    let fixture = parse_llm_mocks_jsonl(
        "{\"schemaVersion\":1,\"strictScopes\":false}\n\
         {\"id\":\"one\",\"scope\":\"custom_review\",\"consume\":\"once\",\"text\":\"ONE\"}\n\
         {\"id\":\"two\",\"scope\":\"custom_review\",\"consume\":\"once\",\"text\":\"TWO\"}\n",
    )
    .expect("open scope strings remain valid");
    assert_eq!(fixture.warnings.len(), 1);
    assert!(fixture.warnings[0].contains("custom_review"));
    assert!(fixture.warnings[0].contains("completion.judge"));
}

#[test]
fn load_v0_fixture_has_no_header_and_default_scope() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("fixture.jsonl");
    std::fs::write(&path, "{\"text\": \"first\"}\n{\"text\": \"second\"}\n")
        .expect("write fixture");
    let fixture = load_llm_mocks_jsonl(&path).expect("load v0");
    assert_eq!(fixture.schema_version, 0);
    assert!(!fixture.strict_scopes);
    assert_eq!(fixture.mocks.len(), 2);
    assert!(fixture.mocks.iter().all(|m| m.scope == DEFAULT_MOCK_SCOPE));
}

#[test]
fn text_parser_is_the_file_parser_contract_owner() {
    let text = "{\"schemaVersion\":1,\"strictScopes\":true}\n\
                {\"id\":\"main-1\",\"scope\":\"agent.main\",\"consume\":\"once\",\"text\":\"MAIN\"}\n";
    let from_text = parse_llm_mocks_jsonl(text).expect("parse text");

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("fixture.jsonl");
    std::fs::write(&path, text).expect("write fixture");
    let from_file = load_llm_mocks_jsonl(&path).expect("parse file");

    assert_eq!(from_text.schema_version, from_file.schema_version);
    assert_eq!(from_text.strict_scopes, from_file.strict_scopes);
    assert_eq!(from_text.mocks[0].entry_id, from_file.mocks[0].entry_id);
    assert_eq!(from_text.mocks[0].scope, from_file.mocks[0].scope);
}

#[test]
fn text_parser_rejects_duplicate_v1_entry_ids_before_installation() {
    let err = parse_llm_mocks_jsonl(
        "{\"schemaVersion\":1,\"strictScopes\":false}\n\
         {\"id\":\"same\",\"scope\":\"main\",\"consume\":\"once\",\"text\":\"one\"}\n\
         {\"id\":\"same\",\"scope\":\"main\",\"consume\":\"once\",\"text\":\"two\"}\n",
    )
    .expect_err("duplicate ids must fail");
    assert!(err.contains("duplicate fixture entry id"), "{err}");
}

#[test]
fn text_parser_rejects_late_header_and_untrimmed_v1_identity() {
    let late_header = parse_llm_mocks_jsonl(
        "{\"text\":\"first\"}\n{\"schemaVersion\":1,\"strictScopes\":false}\n",
    )
    .expect_err("late header must fail");
    assert!(
        late_header.contains("first non-empty line"),
        "{late_header}"
    );

    let untrimmed_id = parse_llm_mocks_jsonl("{\"schemaVersion\":1,\"strictScopes\":false}\n{\"id\":\" judge \",\"scope\":\"main\",\"consume\":\"once\"}\n")
        .expect_err("untrimmed v1 id must fail");
    assert!(
        untrimmed_id.contains("non-empty trimmed string"),
        "{untrimmed_id}"
    );
}
