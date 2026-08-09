//! Deserialization-failure reporting for orchestration payloads.
//!
//! The excerpt in the error message is bounded, and the bound is measured in
//! bytes — so the cut has to land on a character boundary. Slicing the payload
//! at a fixed byte index instead panicked whenever a multi-byte character
//! straddled it, replacing a typed `VmError` with a process abort on the very
//! path that exists to report bad input.

use serde::Deserialize;

use crate::orchestration::parse_json_payload;

#[derive(Debug, Deserialize)]
struct Expected {
    #[allow(dead_code)]
    required_field: String,
}

/// Build a JSON object whose serialized form is longer than the excerpt
/// budget and whose multi-byte characters cover every possible cut position.
fn multibyte_payload(pad_chars: usize) -> serde_json::Value {
    serde_json::json!({ "unexpected": "æ".repeat(pad_chars) })
}

#[test]
fn multibyte_payload_excerpt_reports_a_typed_error_instead_of_panicking() {
    // "æ" is two bytes, so shifting the padding by one walks the byte cut
    // across a character boundary in every possible alignment.
    for pad_chars in 400..460 {
        let error = parse_json_payload::<Expected>(multibyte_payload(pad_chars), "orchestration")
            .expect_err("payload is missing `required_field`");
        let message = format!("{error:?}");
        assert!(
            message.contains("orchestration parse error"),
            "expected a typed parse error, got: {message}"
        );
    }
}

#[test]
fn short_payloads_are_quoted_in_full() {
    let error = parse_json_payload::<Expected>(serde_json::json!({ "unexpected": "æøå" }), "test")
        .expect_err("payload is missing `required_field`");
    let message = format!("{error:?}");
    assert!(message.contains("æøå"), "message was: {message}");
    assert!(!message.contains('…'), "message was: {message}");
}
