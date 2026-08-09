//! `tool_support.parity` provenance in the generated catalog artifact (#5885).

use super::*;

/// #5885: the artifact has to say which kind of thing `parity` is.
///
/// `parity` carries a declaration -- either authored on a capability row or
/// computed from `native`/`text`. A forced-format sweep is a different kind
/// of evidence and lands in `empirical_parity`. Without `parity_source` a
/// consumer could not tell an authored verdict from a computed default, so a
/// guard meant to fire on a finding fired on a fallback.
#[test]
fn tool_support_parity_carries_its_provenance() {
    let artifact = artifact();
    let mut declared = 0;
    let mut derived = 0;
    for model in &artifact.models {
        let support = &model.tool_support;
        match (support.parity.as_deref(), support.parity_source.as_deref()) {
            (Some(_), Some("declared")) => declared += 1,
            (Some(_), Some("derived")) => derived += 1,
            (verdict, source) => panic!(
                "{}/{}: parity {verdict:?} must carry a declared/derived source, got {source:?}",
                model.provider, model.id
            ),
        }
    }
    assert!(declared > 0 && derived > 0, "expected both provenances");

    // The class the issue reported: one verdict reached from both directions.
    let text_only: Vec<_> = artifact
        .models
        .iter()
        .filter(|model| model.tool_support.parity.as_deref() == Some("text_only"))
        .map(|model| model.tool_support.parity_source.as_deref())
        .collect();
    assert!(
        text_only.contains(&Some("declared")) && text_only.contains(&Some("derived")),
        "text_only should still be reachable both ways; that is why the field alone was ambiguous"
    );
}

/// The published schema has to admit the field, or a consumer validating
/// against it would reject a catalog that carries provenance.
#[test]
fn generated_schema_declares_the_parity_source_vocabulary() {
    assert_eq!(
        schema_value()["$defs"]["tool_support"]["properties"]["parity_source"]["enum"],
        serde_json::json!(["declared", "derived"])
    );
}
