//! `compact: true` shortens the description a tool is served, per format.
//!
//! The native wire payload is asserted here because `vm_tools_to_native` *is*
//! the native renderer — there is no layer between it and the provider. The
//! text catalog is asserted end-to-end in
//! `conformance/tests/agents/agent_tool_compact_listing.harn`, where the
//! stdlib renders it into a real system prompt.

use super::{collect_tool_schemas, json, vm_bool, vm_dict, vm_list, vm_str, vm_tools_to_native};
use crate::value::VmValue;

/// The tail exists so a test can prove it was dropped: asserting only that
/// the summary is present would pass just as well with no truncation at all.
const LONG_DESCRIPTION: &str = "Eval-only stop cord. Call this instead of continuing when the \
                                eval fixture, harness, or provided context is missing or broken. \
                                SENTINEL_TAIL: do not use it for normal task difficulty.";
const EXPECTED_SUMMARY: &str = "Eval-only stop cord. Call this instead of continuing when the \
                                eval fixture, harness, or provided context is missing or broken.";

fn mixed_registry() -> VmValue {
    let compact_tool = vm_dict(&[
        ("name", vm_str("stop_harness")),
        ("description", vm_str(LONG_DESCRIPTION)),
        ("compact", vm_bool(true)),
        ("parameters", vm_dict(&[])),
    ]);
    let full_tool = vm_dict(&[
        ("name", vm_str("look")),
        ("description", vm_str(LONG_DESCRIPTION)),
        ("parameters", vm_dict(&[])),
    ]);
    vm_dict(&[
        ("_type", vm_str("tool_registry")),
        ("tools", vm_list(vec![compact_tool, full_tool])),
    ])
}

fn description_of(tools: &[serde_json::Value], name: &str) -> String {
    tools
        .iter()
        .find(|tool| {
            tool.get("name").and_then(serde_json::Value::as_str) == Some(name)
                || tool
                    .pointer("/function/name")
                    .and_then(serde_json::Value::as_str)
                    == Some(name)
        })
        .map(|tool| {
            tool.get("description")
                .or_else(|| tool.pointer("/function/description"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string()
        })
        .unwrap_or_else(|| panic!("{name} must be present in the served tools"))
}

#[test]
fn native_anthropic_serves_the_summary_for_compact_and_the_full_text_otherwise() {
    let tools = vm_tools_to_native(&mixed_registry(), "anthropic", "claude-opus-4-7")
        .expect("anthropic native tools");

    let compact = description_of(&tools, "stop_harness");
    assert_eq!(compact, EXPECTED_SUMMARY);
    assert!(
        !compact.contains("SENTINEL_TAIL"),
        "a compact tool must not ship the tail of its description: {compact:?}"
    );

    // The negative control. Without it a renderer that truncated everything
    // would pass the assertion above.
    assert_eq!(description_of(&tools, "look"), LONG_DESCRIPTION);
}

#[test]
fn native_openai_shape_shortens_the_same_field() {
    let tools =
        vm_tools_to_native(&mixed_registry(), "openai", "gpt-5.3").expect("openai native tools");
    assert_eq!(description_of(&tools, "stop_harness"), EXPECTED_SUMMARY);
    assert_eq!(description_of(&tools, "look"), LONG_DESCRIPTION);
}

#[test]
fn the_catalog_row_keeps_the_full_description_and_the_flag() {
    // The sidecar records what was declared, so a reader can still recover the
    // full text and see why the served copy was shorter. Serving the summary
    // by rewriting `description` here would destroy that.
    let schemas = collect_tool_schemas(Some(&mixed_registry()), None);
    let compact = schemas
        .iter()
        .find(|schema| schema.name == "stop_harness")
        .expect("compact tool collected");
    assert!(compact.summary_only, "the wire key `compact` must be read");
    assert_eq!(compact.description, LONG_DESCRIPTION);
    assert_eq!(compact.served_description(), EXPECTED_SUMMARY);

    let full = schemas
        .iter()
        .find(|schema| schema.name == "look")
        .expect("full tool collected");
    assert!(!full.summary_only);
    assert_eq!(full.served_description(), LONG_DESCRIPTION);
}

#[test]
fn the_serialized_key_stays_compact() {
    // Renaming the Rust field must not rename the wire key: every existing
    // host declaration and every persisted sidecar row spells it `compact`.
    let schemas = collect_tool_schemas(Some(&mixed_registry()), None);
    let row = serde_json::to_value(
        schemas
            .iter()
            .find(|schema| schema.name == "stop_harness")
            .expect("compact tool collected"),
    )
    .expect("row serializes");
    assert_eq!(row.get("compact"), Some(&json!(true)));
    assert!(
        row.get("summary_only").is_none(),
        "the Rust field name must not leak onto the wire: {row}"
    );
}

#[test]
fn the_training_projection_records_the_description_that_was_served() {
    let schemas = collect_tool_schemas(Some(&mixed_registry()), None);
    let row = serde_json::to_value(
        schemas
            .iter()
            .find(|schema| schema.name == "stop_harness")
            .expect("compact tool collected"),
    )
    .expect("row serializes");
    let projected = crate::llm::tools::function_schema_from_catalog_row(&row)
        .expect("catalog row converts to a function schema");
    assert_eq!(
        projected["function"]["description"],
        json!(EXPECTED_SUMMARY),
        "a corpus must teach the description the model read, not the one the catalog stored"
    );
}
