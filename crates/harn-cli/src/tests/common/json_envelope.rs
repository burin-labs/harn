//! Shared assertion helper for [`crate::json_envelope::JsonEnvelope`]
//! payloads. Lives outside `#[cfg(test)]` so the integration tests in
//! `crates/harn-cli/tests/` can reuse it alongside unit tests.

use serde_json::Value;

/// Assert that `value` is a well-formed [`JsonEnvelope`] whose
/// `schemaVersion` matches `expected`. Returns a reference to the
/// `data` field (or `null` on an error envelope) so callers can chain
/// downstream assertions.
///
/// Panics with a clear message when shape invariants are violated:
/// missing `schemaVersion`/`ok`, wrong version, or `ok`/`error`
/// inconsistency.
pub fn assert_envelope(value: &Value, expected_schema_version: u32) -> &Value {
    let sv = value
        .get("schemaVersion")
        .and_then(Value::as_u64)
        .unwrap_or_else(|| panic!("envelope missing `schemaVersion`: {value}"));
    assert_eq!(
        sv as u32, expected_schema_version,
        "schemaVersion mismatch (got {sv}, want {expected_schema_version}) for {value}"
    );

    let ok = value
        .get("ok")
        .and_then(Value::as_bool)
        .unwrap_or_else(|| panic!("envelope missing `ok`: {value}"));

    if ok {
        assert!(
            value.get("error").map(Value::is_null).unwrap_or(true),
            "ok=true but error present: {value}"
        );
    } else {
        let err = value
            .get("error")
            .filter(|v| !v.is_null())
            .unwrap_or_else(|| panic!("ok=false requires `error`: {value}"));
        assert!(
            err.get("code")
                .and_then(Value::as_str)
                .map(|s| !s.is_empty())
                .unwrap_or(false),
            "error.code must be a non-empty string: {value}"
        );
        assert!(
            err.get("message")
                .and_then(Value::as_str)
                .map(|s| !s.is_empty())
                .unwrap_or(false),
            "error.message must be a non-empty string: {value}"
        );
    }

    value.get("data").unwrap_or(&Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn accepts_well_formed_ok_envelope() {
        let v = json!({
            "schemaVersion": 3,
            "ok": true,
            "data": { "value": 1 }
        });
        let data = assert_envelope(&v, 3);
        assert_eq!(data["value"], 1);
    }

    #[test]
    fn accepts_well_formed_error_envelope() {
        let v = json!({
            "schemaVersion": 1,
            "ok": false,
            "error": { "code": "io", "message": "disk full" }
        });
        let data = assert_envelope(&v, 1);
        assert!(data.is_null());
    }

    #[test]
    #[should_panic(expected = "schemaVersion mismatch")]
    fn rejects_version_mismatch() {
        let v = json!({ "schemaVersion": 1, "ok": true, "data": {} });
        assert_envelope(&v, 2);
    }

    #[test]
    #[should_panic(expected = "envelope missing `ok`")]
    fn rejects_missing_ok() {
        let v = json!({ "schemaVersion": 1, "data": {} });
        assert_envelope(&v, 1);
    }

    #[test]
    #[should_panic(expected = "ok=true but error present")]
    fn rejects_ok_with_error() {
        let v = json!({
            "schemaVersion": 1,
            "ok": true,
            "data": {},
            "error": { "code": "x", "message": "y" }
        });
        assert_envelope(&v, 1);
    }

    #[test]
    #[should_panic(expected = "ok=false requires `error`")]
    fn rejects_err_without_payload() {
        let v = json!({ "schemaVersion": 1, "ok": false, "error": null });
        assert_envelope(&v, 1);
    }
}
