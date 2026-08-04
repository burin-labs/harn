//! The single owner of canonical JSON encoding for hash and signature
//! inputs in the VM.
//!
//! Seven near-identical canonicalizers used to live across the VM
//! (`mcp_host`, `channels`, `llm::cache`, `orchestration::lifecycle_receipts`,
//! `orchestration::merge_captain_audit`, `stdlib::project_enrich`,
//! `stdlib::transcript_project`), alongside an eighth in
//! `harn-session-store`. All eight agreed byte-for-byte, but the seven VM
//! copies swallowed serialization errors with `unwrap_or_default()` — a
//! failure would have contributed an *empty* string to a hash rather than
//! surfacing as an error.
//!
//! This module is a thin typed facade over
//! [`harn_session_store::canonical_json_bytes`], which is the lowest layer
//! that already owns this encoding (it produces the bytes that session
//! events are signed over). Delegating rather than reimplementing means
//! the runtime has exactly one canonicalizer, so VM hashes and session
//! signatures cannot drift apart.
//!
//! # Canonicalization rule
//!
//! - Object keys are sorted by their UTF-8 byte order, recursively.
//! - Array order is **preserved** — element order is semantically
//!   meaningful and must never be sorted.
//! - Strings and numbers are encoded by `serde_json`, so escaping and
//!   number formatting match the rest of the runtime.
//! - No insignificant whitespace.
//!
//! # Why an internal owner and not a canonical-JSON crate?
//!
//! Our inputs are open-domain JSON — MCP tool arguments from third-party
//! servers, agent transcripts, channel payloads, resumption inputs, and LLM
//! cache keys — and they provably carry **both** floating-point numbers (the
//! LLM cache key hashes `temperature`, `top_p`, and the penalty options) and
//! integers beyond 2^53 (64-bit ids, nanosecond timestamps). Every maintained
//! canonical-JSON crate was evaluated empirically and none spans that domain:
//!
//! - **`serde_jcs` (RFC 8785 / JCS)** re-encodes every number through an
//!   IEEE-754 double, silently rounding integers past 2^53 — `u64::MAX`
//!   becomes `18446744073709552000` and `2^53 + 1` collapses onto `2^53`. In
//!   a signing canonicalizer that turns distinct payloads into one signature:
//!   a collision/forgery surface. Disqualifying. (Pinned by the
//!   `adjacent_integers_beyond_2_pow_53_stay_distinct_and_exact` test.)
//! - **`olpc-cjson` (OLPC / TUF signing)** keeps integers exact and sorts
//!   keys byte-wise like us, but *rejects floats by design* (`Err: floating
//!   point numbers are not allowed`) — it would throw on `temperature: 0.7`,
//!   i.e. real data. Disqualifying.
//! - **`canonical_json` (Matrix)** accepts floats but re-encodes them in an
//!   exotic exponential grammar (`0.7` -> `7E-1`) and escapes all non-ASCII;
//!   a large, unusual byte change for no benefit here.
//!
//! So the owner delegates all number and string formatting to `serde_json`
//! itself — `itoa` for integers (all 64 bits exact), `ryu` for floats
//! (shortest round-trip), and its standard minimal escaping. The only bespoke
//! logic is the recursive key sort in
//! [`harn_session_store::canonical_json_bytes`]. Key order is **UTF-8 byte
//! order**; this is a decided fact, pinned by
//! `astral_plane_keys_sort_by_utf8_bytes_not_utf16_code_units`.
//!
//! Because bytes are unchanged from every prior copy, hashes that are
//! **persisted and verified later** stay valid — HMAC-signed session-event and
//! channel emit receipts on the durable event log, `ResumptionReceipt::input_hash`
//! (re-checked on replay, `HARN-SUS-011`), transcript `prefix_hash` session
//! chaining, and on-disk enrichment cache filenames.

use serde::Serialize;
use serde_json::Value as JsonValue;

/// Canonical JSON bytes for a value that is already a [`JsonValue`].
pub fn to_vec(value: &JsonValue) -> Vec<u8> {
    harn_session_store::canonical_json_bytes(value)
}

/// Canonical JSON text for a value that is already a [`JsonValue`].
///
/// Total: encoding a `JsonValue` as JSON text cannot fail, and the
/// encoder emits `serde_json` string output, which is always valid
/// UTF-8. The `expect` therefore documents an unreachable branch — and,
/// unlike the `unwrap_or_default()` it replaces, it can never silently
/// feed an empty string into a hash or signature.
pub fn to_string(value: &JsonValue) -> String {
    String::from_utf8(to_vec(value)).expect("canonical JSON encoding is always valid UTF-8")
}

/// Canonical JSON text for any [`Serialize`] type.
///
/// This is the genuinely fallible boundary: converting an arbitrary
/// `Serialize` implementation into a [`JsonValue`] can fail (non-finite
/// floats, non-string map keys, a `Serialize` impl that errors). Callers
/// must propagate the error rather than hashing a placeholder.
pub fn of<T: Serialize + ?Sized>(value: &T) -> Result<String, serde_json::Error> {
    serde_json::to_value(value).map(|value| to_string(&value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn object_keys_are_sorted_regardless_of_insertion_order() {
        assert_eq!(to_string(&json!({"b": 1, "a": 2})), r#"{"a":2,"b":1}"#);
        assert_eq!(
            to_string(&json!({"b": 1, "a": 2})),
            to_string(&json!({"a": 2, "b": 1}))
        );
    }

    #[test]
    fn sorting_does_not_depend_on_serde_map_iteration_order() {
        // Guard against a dependency enabling `serde_json/preserve_order`
        // (Cargo features are additive workspace-wide): insertion order here
        // is deliberately reversed, and the output must still be sorted even
        // if `Map` becomes insertion-ordered.
        let mut map = serde_json::Map::new();
        map.insert("z".to_string(), json!(1));
        map.insert("m".to_string(), json!(2));
        map.insert("a".to_string(), json!(3));
        assert_eq!(to_string(&JsonValue::Object(map)), r#"{"a":3,"m":2,"z":1}"#);
    }

    #[test]
    fn nested_objects_are_sorted_at_every_depth() {
        assert_eq!(
            to_string(&json!({"z": {"y": 1, "x": {"b": 2, "a": 3}}, "a": null})),
            r#"{"a":null,"z":{"x":{"a":3,"b":2},"y":1}}"#
        );
    }

    #[test]
    fn array_order_is_preserved_and_elements_are_canonicalized() {
        assert_eq!(to_string(&json!([3, 1, 2])), "[3,1,2]");
        assert_eq!(
            to_string(&json!([{"b": 1, "a": 2}, {"d": 4, "c": 3}])),
            r#"[{"a":2,"b":1},{"c":3,"d":4}]"#
        );
    }

    #[test]
    fn keys_are_ordered_by_utf8_bytes_and_escaped_by_serde() {
        assert_eq!(
            to_string(&json!({"": 1, " ": 2, "\u{1}": 3, "é": 4, "\"q\"": 5, "a\\b": 6})),
            "{\"\":1,\"\\u0001\":3,\" \":2,\"\\\"q\\\"\":5,\"a\\\\b\":6,\"é\":4}"
        );
        // Digits sort before uppercase, uppercase before `_`, `_` before
        // lowercase — plain UTF-8 byte order, not a locale collation.
        assert_eq!(
            to_string(&json!({"a": 1, "A": 2, "0": 3, "_": 4})),
            r#"{"0":3,"A":2,"_":4,"a":1}"#
        );
    }

    #[test]
    fn scalars_and_empty_containers_round_trip() {
        for value in [
            json!(null),
            json!(true),
            json!(42),
            json!("bare"),
            json!([]),
            json!({}),
        ] {
            assert_eq!(to_string(&value), serde_json::to_string(&value).unwrap());
        }
    }

    #[test]
    fn numbers_keep_serde_json_formatting_not_rfc_8785() {
        // Pins the compatibility decision: RFC 8785 would emit `1` and `0`
        // here, invalidating every persisted hash. See the module docs.
        assert_eq!(
            to_string(&json!({"n": [1.0, -0.0, 1.5]})),
            r#"{"n":[1.0,-0.0,1.5]}"#
        );
    }

    /// The test that MUST scream if anyone ever swaps this owner for RFC 8785
    /// / `serde_jcs`. JCS routes every number through an IEEE-754 double,
    /// silently rounding integers past 2^53: it maps both 2^53 and 2^53+1 to
    /// `9007199254740992`, and `u64::MAX` to `18446744073709552000`. Because
    /// this canonicalizer feeds signatures and hash keys over payloads that
    /// carry 64-bit ids and nanosecond timestamps, a lossy encoder would
    /// collapse two distinct payloads to one hash — a collision/forgery
    /// surface. serde_json serializes integers via `itoa`, so all 64 bits
    /// survive. See the module docs for the full alternatives analysis.
    #[test]
    fn adjacent_integers_beyond_2_pow_53_stay_distinct_and_exact() {
        let two_pow_53 = 9007199254740992_i64;
        let two_pow_53_plus_1 = 9007199254740993_i64;

        // Distinct inputs MUST yield distinct canonical bytes (JCS fails here).
        assert_ne!(
            to_string(&json!(two_pow_53)),
            to_string(&json!(two_pow_53_plus_1))
        );
        // Each round-trips exactly through the canonical form.
        assert_eq!(to_string(&json!(two_pow_53)), "9007199254740992");
        assert_eq!(to_string(&json!(two_pow_53_plus_1)), "9007199254740993");

        // 64-bit endpoints survive verbatim (JCS rounds u64::MAX).
        assert_eq!(to_string(&json!(u64::MAX)), "18446744073709551615");
        assert_eq!(to_string(&json!(i64::MIN)), "-9223372036854775808");
        assert_eq!(
            to_string(&json!({"n": 12345678901234567890u64})),
            r#"{"n":12345678901234567890}"#
        );
    }

    #[test]
    fn astral_plane_keys_sort_by_utf8_bytes_not_utf16_code_units() {
        // Decides the UTF-8-vs-UTF-16 key-order ambiguity as a fact: we sort by
        // UTF-8 bytes (U+FFFF's 0xEF.. precedes U+1F600's 0xF0..), so the emoji
        // key comes LAST. RFC 8785 sorts by UTF-16 code units, where the
        // surrogate-pair lead 0xD83D precedes 0xFFFF and the emoji would come
        // first — a second, quieter way JCS would change these bytes.
        assert_eq!(
            to_string(&json!({"\u{1f600}": 1, "\u{ffff}": 2, "z": 3})),
            "{\"z\":3,\"\u{ffff}\":2,\"\u{1f600}\":1}"
        );
    }

    #[test]
    fn to_vec_matches_to_string_bytes() {
        let value = json!({"b": [1, {"d": 4, "c": 3}], "a": "x"});
        assert_eq!(to_vec(&value), to_string(&value).into_bytes());
    }

    #[test]
    fn of_canonicalizes_serializable_values() {
        #[derive(Serialize)]
        struct Identity {
            z: u32,
            a: &'static str,
        }
        assert_eq!(
            of(&Identity { z: 1, a: "x" }).unwrap(),
            r#"{"a":"x","z":1}"#
        );
    }

    #[test]
    fn of_surfaces_serialization_failure_instead_of_an_empty_hash_input() {
        // A map with non-string keys cannot become a JSON object. The old
        // `unwrap_or_default()` call sites would have hashed "" here.
        let mut bad = std::collections::BTreeMap::new();
        bad.insert(vec![1_u8, 2], "value");
        let error = of(&bad).expect_err("non-string map keys must not serialize");
        assert!(
            !error.to_string().is_empty(),
            "error must describe the failure"
        );
    }
}
