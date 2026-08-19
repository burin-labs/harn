//! `tool_support.parity` provenance in the generated catalog artifact (#5885).

use std::collections::{BTreeMap, BTreeSet};

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

    // The class the issue reported: one verdict reached from both directions,
    // which is why the verdict alone was ambiguous without its source. Asserted
    // over whichever verdict is currently dual-sourced rather than a named one,
    // because which verdict that is follows the catalog data and changes
    // whenever a row is re-receipted, while the ambiguity class does not.
    let mut sources_by_verdict: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for model in &artifact.models {
        let support = &model.tool_support;
        if let (Some(verdict), Some(source)) =
            (support.parity.as_deref(), support.parity_source.as_deref())
        {
            sources_by_verdict
                .entry(verdict)
                .or_default()
                .insert(source);
        }
    }
    assert!(
        sources_by_verdict
            .values()
            .any(|sources| sources.contains("declared") && sources.contains("derived")),
        "at least one verdict must be reachable both ways, or this catalog no longer \
         exercises the ambiguity that made parity_source necessary; got {sources_by_verdict:?}"
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
