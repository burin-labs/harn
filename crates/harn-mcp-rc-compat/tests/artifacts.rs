//! Downstream-consumer fixture parity + recursive `$defs` validation.
//!
//! Two acceptance bars live here:
//!
//! 1. Every wire fixture published under
//!    `spec/protocol-artifacts/fixtures/mcp-rc/` parses, has the
//!    required metadata fields (name, description, kind, documents),
//!    and round-trips through Harn's helper API where applicable.
//! 2. At least one fixture uses a JSON Schema 2020-12 schema with
//!    recursive `$defs` so the dialect handling is exercised end-to-end
//!    by `jsonschema::draft202012`. The recursive case is the one that
//!    has historically tripped pre-2020-12 validators.
//!
//! Failures here localize to the **artifact / downstream-consumer**
//! surface.

use harn_mcp_rc_compat::fixtures::{all_fixtures, load_named, WireFixtureKind};
use harn_mcp_rc_compat::recursive_schema::{
    invalid_tree_instance, recursive_tree_input_schema, valid_tree_instance, validate,
};
use serde_json::{json, Value as JsonValue};

#[test]
fn every_published_fixture_loads_with_required_metadata() {
    let fixtures = all_fixtures();
    assert!(
        fixtures.len() >= 7,
        "expected at least seven RC fixtures, got {}",
        fixtures.len()
    );
    for fixture in fixtures {
        assert!(
            !fixture.name.is_empty(),
            "fixture missing name: {fixture:?}"
        );
        assert!(
            !fixture.description.is_empty(),
            "fixture {} missing description",
            fixture.name
        );
        assert!(
            !fixture.documents.is_empty(),
            "fixture {} has no documents",
            fixture.name
        );
        assert!(
            fixture.name.starts_with("harn.mcp_rc."),
            "fixture name {} must use the harn.mcp_rc.* namespace",
            fixture.name
        );
    }
}

#[test]
fn recursive_defs_schema_validates_valid_tree() {
    validate(&valid_tree_instance()).expect("valid tree must pass draft 2020-12 validation");
}

#[test]
fn recursive_defs_schema_rejects_missing_required_field() {
    let err = validate(&invalid_tree_instance())
        .expect_err("missing required `name` must fail validation");
    assert!(
        err.contains("name") || err.to_lowercase().contains("required"),
        "expected validator to mention the missing required field, got {err}"
    );
}

#[test]
fn recursive_defs_schema_carries_self_referential_ref() {
    let schema = recursive_tree_input_schema();
    let self_ref = &schema["$defs"]["Node"]["properties"]["children"]["items"]["$ref"];
    assert_eq!(self_ref, &json!("#/$defs/Node"));
    assert_eq!(
        schema["$schema"],
        json!("https://json-schema.org/draft/2020-12/schema"),
        "fixture must pin draft 2020-12 so callers know which dialect they're consuming"
    );
}

#[test]
fn published_recursive_schema_fixture_matches_harness_helper() {
    let fixture = load_named("recursive_schema_tool.json");
    assert_eq!(fixture.kind, WireFixtureKind::Schema);
    assert_eq!(fixture.documents.len(), 1);
    let published = &fixture.documents[0];
    let helper = recursive_tree_input_schema();
    // The published fixture must round-trip with the helper byte-for-byte
    // so downstream consumers vendoring the JSON file see the same shape
    // Harn validates against internally.
    assert_eq!(
        published, &helper,
        "published fixture drifted from harness helper; rerun the test to refresh"
    );
}

#[test]
fn modern_success_fixture_has_three_round_trips() {
    let fixture = load_named("modern_success.json");
    assert_eq!(fixture.kind, WireFixtureKind::Exchange);
    // 3 request/response pairs = 6 documents (discover, list, call).
    assert_eq!(
        fixture.documents.len(),
        6,
        "modern_success must cover discover + list + call"
    );
    let methods: Vec<&str> = fixture
        .documents
        .iter()
        .step_by(2)
        .filter_map(|doc| doc.get("method").and_then(JsonValue::as_str))
        .collect();
    assert_eq!(methods, vec!["server/discover", "tools/list", "tools/call"]);
}

#[test]
fn unsupported_version_retry_fixture_demonstrates_first_failure_then_retry() {
    let fixture = load_named("unsupported_version_retry.json");
    // First response must be -32004.
    assert_eq!(fixture.documents[1]["error"]["code"], json!(-32004));
    // Second response must succeed.
    assert!(fixture.documents[3]["result"].is_object());
}

#[test]
fn header_mismatch_fixture_is_http_header_exchange() {
    let fixture = load_named("header_mismatch.json");
    assert_eq!(fixture.kind, WireFixtureKind::HttpHeaderExchange);
    let headers = &fixture.documents[0];
    let body = &fixture.documents[1];
    assert_eq!(headers["Mcp-Method"], json!("tools/list"));
    assert_eq!(body["method"], json!("tools/call"));
}

#[test]
fn input_required_fixture_uses_resolved_request_state_on_retry() {
    let fixture = load_named("input_required.json");
    // First response is input-required; second request must echo
    // the requestState from the first.
    let first_state = &fixture.documents[1]["result"]["requestState"];
    let retry_state = &fixture.documents[2]["params"]["requestState"];
    assert!(first_state.is_string());
    assert_eq!(first_state, retry_state);
}
