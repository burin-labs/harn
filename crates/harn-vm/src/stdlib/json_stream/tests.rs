use super::*;

fn string(value: &str) -> VmValue {
    VmValue::string(value)
}

fn dict(entries: impl IntoIterator<Item = (&'static str, VmValue)>) -> VmValue {
    VmValue::dict(
        entries
            .into_iter()
            .map(|(key, value)| (crate::value::intern_key(key), value)),
    )
}

fn list(items: Vec<VmValue>) -> VmValue {
    VmValue::list(items)
}

fn status_variant(value: &VmValue) -> String {
    match value {
        VmValue::EnumVariant(enum_variant) => enum_variant.variant.to_string(),
        other => panic!("expected enum status, got {other:?}"),
    }
}

fn call(name: &str, args: Vec<VmValue>) -> VmValue {
    call_result(name, args).unwrap()
}

fn call_result(name: &str, args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let mut vm = crate::new_vm();
    register_json_stream_builtins(&mut vm);
    let builtin = vm.builtin_handler_for_test(name).unwrap().clone();
    builtin(&args, &mut String::new())
}

#[test]
fn validator_authority_cannot_be_forged_with_a_string() {
    let error = call_result(
        "__json_stream_validator_feed",
        vec![string("json_stream_validator:1"), string("{}")],
    )
    .expect_err("script-visible strings must not act as validator authority");
    assert!(error.to_string().contains("expected validator handle"));
}

#[test]
fn stream_validator_rejects_cyclic_schema_at_create() {
    reset_json_stream_state();
    let schema = dict([("$ref", string("#"))]);
    let error = call_result("__json_stream_validator", vec![schema])
        .expect_err("cyclic schema must be rejected");

    assert!(
        error
            .to_string()
            .contains("cyclic schema reference: # -> #"),
        "expected cyclic schema reference error, got {error:?}"
    );
}

#[test]
fn feed_reaches_valid_once_document_completes() {
    reset_json_stream_state();
    let schema = dict([
        ("type", string("object")),
        ("required", list(vec![string("name")])),
        (
            "properties",
            dict([("name", dict([("type", string("string"))]))]),
        ),
    ]);
    let handle = call("__json_stream_validator", vec![schema]);
    let pending = call(
        "__json_stream_validator_feed",
        vec![handle.clone(), string("{\"name\"")],
    );
    assert_eq!(status_variant(&pending), "Pending");
    let valid = call(
        "__json_stream_validator_feed",
        vec![handle.clone(), string(":\"Ada\"}")],
    );
    assert_eq!(status_variant(&valid), "Valid");
    let value = call("__json_stream_validator_value", vec![handle]);
    assert!(matches!(value, VmValue::Dict(_)));
}

#[test]
fn detects_impossible_property_type_before_object_closes() {
    reset_json_stream_state();
    let schema = dict([
        ("type", string("object")),
        ("required", list(vec![string("age")])),
        (
            "properties",
            dict([("age", dict([("type", string("int"))]))]),
        ),
    ]);
    let handle = call("__json_stream_validator", vec![schema]);
    let invalid = call(
        "__json_stream_validator_feed",
        vec![handle, string("{\"age\":\"")],
    );
    assert_eq!(status_variant(&invalid), "Invalid");
}

#[test]
fn feed_after_valid_non_whitespace_invalidates_stream() {
    reset_json_stream_state();
    let schema = dict([("type", string("int"))]);
    let handle = call("__json_stream_validator", vec![schema]);
    let valid = call(
        "__json_stream_validator_feed",
        vec![handle.clone(), string("1 ")],
    );
    assert_eq!(status_variant(&valid), "Valid");
    let invalid = call("__json_stream_validator_feed", vec![handle, string("2")]);
    assert_eq!(status_variant(&invalid), "Invalid");
}

#[test]
fn stream_validate_verdict_progresses_pending_to_valid() {
    reset_json_stream_state();
    let schema = dict([
        ("type", string("dict")),
        ("required", list(vec![string("name")])),
        (
            "properties",
            dict([("name", dict([("type", string("string"))]))]),
        ),
    ]);
    let handle = call("__json_stream_validate_create", vec![schema]);
    let pending = call(
        "__json_stream_validate_chunk",
        vec![handle.clone(), string("{\"name\":")],
    );
    let pending_map = pending.as_dict().expect("pending dict");
    assert_eq!(
        pending_map.get("verdict").map(VmValue::display).as_deref(),
        Some("pending")
    );
    let valid = call(
        "__json_stream_validate_chunk",
        vec![handle, string("\"Ada\"}")],
    );
    let valid_map = valid.as_dict().expect("valid dict");
    assert_eq!(
        valid_map.get("verdict").map(VmValue::display).as_deref(),
        Some("valid")
    );
}

#[test]
fn stream_validate_chunk_surfaces_invalid_reason_and_path() {
    reset_json_stream_state();
    let schema = dict([
        ("type", string("dict")),
        ("required", list(vec![string("age")])),
        (
            "properties",
            dict([("age", dict([("type", string("int"))]))]),
        ),
    ]);
    let handle = call("__json_stream_validate_create", vec![schema]);
    let invalid = call(
        "__json_stream_validate_chunk",
        vec![handle, string("{\"age\":\"")],
    );
    let invalid_map = invalid.as_dict().expect("invalid dict");
    assert_eq!(
        invalid_map.get("verdict").map(VmValue::display).as_deref(),
        Some("invalid")
    );
    assert_eq!(
        invalid_map
            .get("reason_kind")
            .map(VmValue::display)
            .as_deref(),
        Some("wrong_type")
    );
    assert!(invalid_map.contains_key("reason"));
    assert!(invalid_map.contains_key("path"));
}

#[test]
fn stream_validate_finalize_invalidates_incomplete_partial() {
    reset_json_stream_state();
    let schema = dict([("type", string("dict"))]);
    let handle = call("__json_stream_validate_create", vec![schema]);
    let pending = call(
        "__json_stream_validate_chunk",
        vec![handle.clone(), string("{\"a\":")],
    );
    assert_eq!(
        pending
            .as_dict()
            .and_then(|d| d.get("verdict"))
            .map(VmValue::display)
            .as_deref(),
        Some("pending")
    );
    let finalized = call("__json_stream_validate_finalize", vec![handle]);
    let finalized_map = finalized.as_dict().expect("finalize dict");
    assert_eq!(
        finalized_map
            .get("verdict")
            .map(VmValue::display)
            .as_deref(),
        Some("invalid")
    );
    assert_eq!(
        finalized_map.get("reason").map(VmValue::display).as_deref(),
        Some("incomplete JSON document at end of stream")
    );
}

#[test]
fn stream_validate_finalize_keeps_valid_intact() {
    reset_json_stream_state();
    let schema = dict([("type", string("int"))]);
    let handle = call("__json_stream_validate_create", vec![schema]);
    let valid = call(
        "__json_stream_validate_chunk",
        vec![handle.clone(), string("42")],
    );
    assert_eq!(
        valid
            .as_dict()
            .and_then(|d| d.get("verdict"))
            .map(VmValue::display)
            .as_deref(),
        Some("valid")
    );
    let finalized = call("__json_stream_validate_finalize", vec![handle]);
    assert_eq!(
        finalized
            .as_dict()
            .and_then(|d| d.get("verdict"))
            .map(VmValue::display)
            .as_deref(),
        Some("valid")
    );
}

#[test]
fn one_byte_chunks_keep_large_container_linear() {
    let schema = schema::schema_from_json_schema_value(&dict([
        ("type", string("list")),
        ("items", dict([("type", string("int"))])),
    ]))
    .unwrap();
    let mut validator = JsonStreamValidator {
        schema,
        buffer: String::new(),
        pending_utf8: Vec::new(),
        scan: JsonStreamScan::default(),
        status: JsonStreamStatus::Pending,
        value: None,
    };
    let payload = format!("[{}]", vec!["1"; 6_000].join(","));

    for byte in payload.as_bytes() {
        validator.feed(std::slice::from_ref(byte));
    }

    assert_eq!(validator.status, JsonStreamStatus::Valid);
    assert!(matches!(validator.value, Some(VmValue::List(_))));
}

// ---- Markdown code-fence tolerance (schema_stream_aborted fix) ----

/// Object schema requiring an int property `a`, expressed as JSON Schema
/// so we can drive the Send-safe `StreamSchemaValidator` used on the LLM
/// streaming path (the surface the `schema_stream_aborted` bug hits).
fn object_a_int_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "required": ["a"],
        "properties": { "a": { "type": "integer" } },
    })
}

/// Feed `chunks` into a fresh `StreamSchemaValidator` and return the
/// final status. Mirrors how the streaming transport drives the watch.
fn feed_chunks(schema: &serde_json::Value, chunks: &[&str]) -> JsonStreamStatus {
    let mut validator = StreamSchemaValidator::from_json_schema(schema).expect("schema");
    let mut status = JsonStreamStatus::Pending;
    for chunk in chunks {
        status = validator.feed(chunk).clone();
    }
    status
}

#[test]
fn fenced_json_with_language_tag_validates_single_chunk() {
    let status = feed_chunks(&object_a_int_schema(), &["```json\n{\"a\":1}\n```"]);
    assert_eq!(status, JsonStreamStatus::Valid);
}

#[test]
fn fenced_array_without_language_tag_validates() {
    let schema = serde_json::json!({ "type": "array", "items": { "type": "integer" } });
    let status = feed_chunks(&schema, &["```\n[1,2,3]\n```"]);
    assert_eq!(status, JsonStreamStatus::Valid);
}

#[test]
fn fenced_json_survives_awkward_chunk_splits_with_lang_tag() {
    // The fence opener, language tag, and closing fence are split across
    // arbitrary chunk boundaries.
    let status = feed_chunks(
        &object_a_int_schema(),
        &["``", "`js", "on\n{\"a\"", ":1}\n``", "`"],
    );
    assert_eq!(status, JsonStreamStatus::Valid);
}

#[test]
fn fenced_json_survives_single_char_fence_chunks() {
    let schema = serde_json::json!({ "type": "object" });
    let status = feed_chunks(&schema, &["`", "`", "`", "\n", "{}", "\n```"]);
    assert_eq!(status, JsonStreamStatus::Valid);
}

#[test]
fn leading_whitespace_before_fence_is_tolerated() {
    let status = feed_chunks(&object_a_int_schema(), &["  \n ```json\n{\"a\":1}\n```\n"]);
    assert_eq!(status, JsonStreamStatus::Valid);
}

#[test]
fn bare_json_without_fence_still_validates() {
    let status = feed_chunks(&object_a_int_schema(), &["{\"a\":1}"]);
    assert_eq!(status, JsonStreamStatus::Valid);
}

#[test]
fn fenced_root_scalar_validates() {
    let schema = serde_json::json!({ "type": "integer" });
    let status = feed_chunks(&schema, &["```\n42\n```"]);
    assert_eq!(status, JsonStreamStatus::Valid);
}

#[test]
fn non_fence_garbage_lead_still_errors() {
    let status = feed_chunks(&object_a_int_schema(), &["xyz"]);
    assert!(
        matches!(status, JsonStreamStatus::Invalid { .. }),
        "a non-fence, non-JSON lead must stay Invalid, got {status:?}"
    );
}

#[test]
fn incomplete_backtick_run_lead_still_errors() {
    // A single/double backtick not completed to a full ` ``` ` fence
    // before a non-backtick must error rather than silently pass.
    let one = feed_chunks(&object_a_int_schema(), &["`x"]);
    assert!(
        matches!(one, JsonStreamStatus::Invalid { .. }),
        "single backtick + non-backtick must be Invalid, got {one:?}"
    );
    let two = feed_chunks(&object_a_int_schema(), &["``x"]);
    assert!(
        matches!(two, JsonStreamStatus::Invalid { .. }),
        "double backtick + non-backtick must be Invalid, got {two:?}"
    );
}

#[test]
fn schema_invalid_value_inside_fence_still_reports_invalid() {
    // The fence-strip must only affect framing: a value whose type
    // violates the schema (string where an int is required) must still
    // surface as Invalid, not be masked by the fence handling.
    let status = feed_chunks(&object_a_int_schema(), &["```json\n{\"a\":\"oops\"}\n```"]);
    match status {
        JsonStreamStatus::Invalid {
            reason_kind,
            path,
            reason,
        } => {
            assert_eq!(reason_kind, SchemaValidationReasonKind::WrongType);
            assert_eq!(path, "$.a", "unexpected path (reason {reason:?})");
        }
        other => panic!("expected Invalid for schema-violating fenced value, got {other:?}"),
    }
}

#[test]
fn fenced_json_split_after_value_before_closing_fence() {
    // Value completes in one chunk; the closing fence arrives later.
    let status = feed_chunks(&object_a_int_schema(), &["```json\n{\"a\":1}", "\n```"]);
    assert_eq!(status, JsonStreamStatus::Valid);
}

// ---- Root-scalar trailing-junk framing (PR #3098 blocker fix) ----
//
// A root scalar (number / true / false / null) has no closing delimiter,
// so once its value ends at the first trailing whitespace/backtick we must
// FRAME it (`complete = true`) and route every later char through the
// trailing handler. Before the fix, trailing junk after a scalar was
// silently dropped and the stream wrongly accepted as Valid, which would
// let a model emit a valid scalar followed by hallucinated text without
// triggering schema_stream_abort.

fn int_schema() -> serde_json::Value {
    serde_json::json!({ "type": "integer" })
}

#[test]
fn scalar_followed_by_junk_is_invalid() {
    // `42 garbage` was wrongly Valid (body "42") before the fix.
    let status = feed_chunks(&int_schema(), &["42 garbage"]);
    assert!(
        matches!(status, JsonStreamStatus::Invalid { .. }),
        "trailing junk after a root scalar must be Invalid, got {status:?}"
    );
}

#[test]
fn scalar_split_by_space_is_invalid_not_misframed() {
    // `4 2` must NOT parse as 4 (the " 2" being dropped is a mis-frame
    // that hands the caller the wrong value); it is trailing garbage.
    let status = feed_chunks(&int_schema(), &["4 2"]);
    assert!(
        matches!(status, JsonStreamStatus::Invalid { .. }),
        "`4 2` must be Invalid (mis-frame guard), got {status:?}"
    );
}

#[test]
fn bool_followed_by_junk_is_invalid() {
    let schema = serde_json::json!({ "type": "bool" });
    let status = feed_chunks(&schema, &["true nope"]);
    assert!(
        matches!(status, JsonStreamStatus::Invalid { .. }),
        "trailing junk after `true` must be Invalid, got {status:?}"
    );
}

#[test]
fn null_followed_by_junk_is_invalid() {
    let schema = serde_json::json!({ "type": "nil" });
    let status = feed_chunks(&schema, &["null xxx"]);
    assert!(
        matches!(status, JsonStreamStatus::Invalid { .. }),
        "trailing junk after `null` must be Invalid, got {status:?}"
    );
}

#[test]
fn scalar_split_by_tab_is_invalid() {
    // Whitespace other than a space (here a tab) frames the scalar too;
    // `7\t9` is trailing garbage, not the number 7.
    let status = feed_chunks(&int_schema(), &["7\t9"]);
    assert!(
        matches!(status, JsonStreamStatus::Invalid { .. }),
        "`7\\t9` must be Invalid, got {status:?}"
    );
}

#[test]
fn fenced_mid_stream_scalar_with_junk_is_not_valid() {
    // Fenced opener + scalar + trailing junk, with no closing fence yet:
    // must NOT be accepted as Valid (body "42") mid-stream.
    let status = feed_chunks(&int_schema(), &["```\n42 garbage"]);
    assert!(
        !matches!(status, JsonStreamStatus::Valid),
        "fenced mid-stream scalar + junk must not be Valid, got {status:?}"
    );
}

#[test]
fn bare_scalar_then_finalize_is_valid() {
    // Regression guard: a bare valid scalar with no trailing chars stays
    // Valid (and finalize keeps it Valid).
    let mut validator = StreamSchemaValidator::from_json_schema(&int_schema()).expect("schema");
    assert_eq!(validator.feed("42").clone(), JsonStreamStatus::Valid);
}

#[test]
fn fenced_scalar_stays_valid() {
    // Regression guard: a properly fenced scalar still validates.
    let status = feed_chunks(&int_schema(), &["```\n42\n```"]);
    assert_eq!(status, JsonStreamStatus::Valid);
}

#[test]
fn multi_token_scalar_run_is_invalid() {
    // Regression guard: `1 2 3` is trailing garbage after the scalar `1`.
    let status = feed_chunks(&int_schema(), &["1 2 3"]);
    assert!(
        matches!(status, JsonStreamStatus::Invalid { .. }),
        "`1 2 3` must be Invalid, got {status:?}"
    );
}

fn partial_json(body: &str) -> serde_json::Value {
    crate::llm::helpers::vm_value_to_json(&best_effort_partial(body))
}

#[test]
fn partial_drops_incomplete_trailing_key() {
    // A half-typed trailing key with no value is dropped, not corrupted.
    assert_eq!(
        partial_json("{\"name\":\"Ada\",\"ag"),
        serde_json::json!({"name": "Ada"})
    );
}

#[test]
fn partial_keeps_incomplete_trailing_value() {
    // A half-typed string *value* is kept as-is (it grows across chunks);
    // the key-vs-value distinction falls out of serde_json validity.
    assert_eq!(
        partial_json("{\"name\":\"Ad"),
        serde_json::json!({"name": "Ad"})
    );
    // A complete value without a trailing comma is included.
    assert_eq!(
        partial_json("{\"name\":\"Ada\",\"age\":3"),
        serde_json::json!({"name": "Ada", "age": 3})
    );
}

#[test]
fn partial_grows_monotonically_to_final() {
    // Empty object frontier, then each complete member, then the finished
    // document — the partial only ever gains keys.
    assert_eq!(partial_json("{"), serde_json::json!({}));
    assert_eq!(partial_json("{\"a\":1,"), serde_json::json!({"a": 1}));
    assert_eq!(
        partial_json("{\"a\":1,\"b\":[1,2"),
        serde_json::json!({"a": 1, "b": [1, 2]})
    );
    assert_eq!(
        partial_json("{\"a\":1,\"b\":[1,2]}"),
        serde_json::json!({"a": 1, "b": [1, 2]})
    );
}

#[test]
fn partial_ignores_string_internal_punctuation() {
    // Commas/braces inside a string value must not be treated as structure.
    assert_eq!(
        partial_json("{\"note\":\"a, b, {c}\",\"x"),
        serde_json::json!({"note": "a, b, {c}"})
    );
}

#[test]
fn partial_empty_buffer_is_nil() {
    assert!(matches!(best_effort_partial("   "), VmValue::Nil));
}

#[test]
fn partial_skips_frontier_search_above_size_cap() {
    // An incomplete document larger than the cap returns Nil rather than
    // running the quadratic frontier search. (A *complete* large document
    // still parses via the fast path; only the incomplete-search is gated.)
    let huge = format!("{{\"a\":\"{}", "x".repeat(300_000));
    assert!(matches!(best_effort_partial(&huge), VmValue::Nil));
}

#[test]
fn partial_value_builtin_matches_helper() {
    // End-to-end through the builtin + validator storage.
    let schema = dict([("type", string("object"))]);
    let handle = call("__json_stream_validator", vec![schema]);
    call(
        "__json_stream_validator_feed",
        vec![handle.clone(), string("{\"name\":\"Ada\",\"ag")],
    );
    let partial = call("__json_stream_validator_partial", vec![handle]);
    assert_eq!(
        crate::llm::helpers::vm_value_to_json(&partial),
        serde_json::json!({"name": "Ada"})
    );
}
